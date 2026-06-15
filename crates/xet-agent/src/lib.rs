//! `xet-agent` — the client/ingest pipeline (`Prompt.md` §7, §8).
//!
//! M0/M1 implement a working dedup'd ingest→reconstruct round-trip using the real Xet
//! primitives (Gearhash CDC, BLAKE3 hashing, xorb serialization, file hash) from the
//! vendored fork. Ingest resolves each chunk against the server's global dedup index
//! (`GET /chunks`, tier 3 of §7.1) and references existing xorbs for known runs, uploading
//! only novel runs — so a localized edit re-uploads only the perturbed neighborhood.
//!
//! File registration uses the server's M0-internal `POST /files` JSON; the binary `mdb_shard`
//! wire format (stock-client interop), the local session/shard caches (tiers 1–2), and the
//! minimum-dedup-run fragmentation control (§7.3) are later refinements.

use std::io::Cursor;
use std::path::Path;

use anyhow::{ensure, Context, Result};
use serde_json::json;
use xet_core::cas_object::{deserialize_chunk, RawXorbData, SerializedXorbObject};
use xet_core::deduplication::Chunker;
use xet_core::merklehash::file_hash;

/// Pack chunks into a xorb up to these bounds (conservatively under `MAX_XORB_SIZE` = 64 MiB).
const MAX_XORB_UNCOMPRESSED: usize = 60 * 1024 * 1024;
const MAX_XORB_CHUNKS: usize = 8192;

/// Attach the `XETD_TOKEN` bearer (if set) so the agent authenticates against a tokens-mode
/// server; a no-op in trusted-loopback deployments.
fn auth(rb: reqwest::blocking::RequestBuilder) -> reqwest::blocking::RequestBuilder {
    match std::env::var("XETD_TOKEN") {
        Ok(t) if !t.is_empty() => rb.bearer_auth(t),
        _ => rb,
    }
}

/// Outcome of an ingest — carries the file's XET-string content hash.
pub struct Ingested {
    file_hash_hex: String,
}

impl Ingested {
    pub fn file_hash_hex(&self) -> String {
        self.file_hash_hex.clone()
    }
}

/// Where a chunk already lives, per the global dedup index: `(xorb_hash_hex, chunk_index)`.
type ChunkLoc = (String, u32);

/// Chunk, deduplicate against the server, upload novel xorbs, then register the file.
/// Returns the file's content hash (deterministic for identical bytes).
pub fn ingest_bytes(base_url: &str, volume: &str, dest: &str, bytes: &[u8], _scratch: &Path) -> Result<Ingested> {
    // 1. Content-defined chunking; each Chunk carries its BLAKE3 hash + data.
    let chunks = Chunker::default().next_block(bytes, true);

    // 2. Whole-file hash over the ordered (chunk_hash, uncompressed_size) list.
    let file_pairs: Vec<_> = chunks.iter().map(|c| (c.hash, c.data.len() as u64)).collect();
    let fh = file_hash(&file_pairs);

    let client = reqwest::blocking::Client::new();

    // 3. Tier 3: resolve every chunk against the global dedup index up front.
    let locs: Vec<Option<ChunkLoc>> = chunks
        .iter()
        .map(|c| query_chunk(&client, base_url, &c.hash.hex()))
        .collect::<Result<_>>()?;

    // 4. Walk the chunk list, emitting one term per run: a deduped reference to an existing
    //    xorb (contiguous, same xorb, consecutive indices) or a freshly uploaded novel xorb.
    let mut terms = Vec::new();
    let mut i = 0;
    while i < chunks.len() {
        match &locs[i] {
            Some((xorb, idx)) => {
                let start_idx = *idx;
                let mut j = i + 1;
                let mut expected = idx + 1;
                while j < chunks.len() {
                    match &locs[j] {
                        Some((x2, i2)) if x2 == xorb && *i2 == expected => {
                            expected += 1;
                            j += 1;
                        }
                        _ => break,
                    }
                }
                let unpacked: u64 = chunks[i..j].iter().map(|c| c.data.len() as u64).sum();
                terms.push(json!({ "xorb": xorb, "start": start_idx, "end": expected, "unpacked_length": unpacked }));
                i = j;
            }
            None => {
                // Gather a contiguous novel run, bounded by the xorb caps, then pack + upload it.
                let mut j = i;
                let mut acc = 0usize;
                while j < chunks.len()
                    && locs[j].is_none()
                    && (j - i) < MAX_XORB_CHUNKS
                    && acc + chunks[j].data.len() <= MAX_XORB_UNCOMPRESSED
                {
                    acc += chunks[j].data.len();
                    j += 1;
                }
                if j == i {
                    j = i + 1; // a single chunk larger than the cap still gets its own xorb
                }
                let group = &chunks[i..j];
                let raw = RawXorbData::from_chunks(group, vec![0]);
                let serialized =
                    SerializedXorbObject::from_xorb(raw, /*footer=*/ true, "lz4", 0).context("serialize xorb")?;
                let xorb_hex = serialized.hash.hex();
                upload_xorb(&client, base_url, &xorb_hex, serialized.serialized_data)?;

                let unpacked: u64 = group.iter().map(|c| c.data.len() as u64).sum();
                terms.push(json!({ "xorb": xorb_hex, "start": 0, "end": group.len(), "unpacked_length": unpacked }));
                i = j;
            }
        }
    }

    // 5. Register the file (terms + VFS catalog entry).
    let total_size: u64 = chunks.iter().map(|c| c.data.len() as u64).sum();
    let resp = auth(client.post(format!("{base_url}/api/v1/files")).json(&json!({
        "file_hash": fh.hex(),
        "total_size": total_size,
        "terms": terms,
        "volume": volume,
        "path": dest,
    })))
    .send()
    .context("register file")?;
    ensure!(resp.status().is_success(), "file registration failed: HTTP {}", resp.status());

    Ok(Ingested { file_hash_hex: fh.hex() })
}

/// Query the global dedup index for one chunk. `Some(loc)` on a hit, `None` on a `404` miss.
fn query_chunk(client: &reqwest::blocking::Client, base_url: &str, hash_hex: &str) -> Result<Option<ChunkLoc>> {
    let resp = auth(client.get(format!("{base_url}/api/v1/chunks/default-merkledb/{hash_hex}")))
        .send()
        .context("chunk dedup query")?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    ensure!(resp.status().is_success(), "chunk query failed: HTTP {}", resp.status());
    let v: serde_json::Value = resp.json().context("parse chunk response")?;
    let xorb = v["xorb"].as_str().context("chunk response missing xorb")?.to_string();
    let index = v["chunk_index"].as_u64().context("chunk response missing chunk_index")? as u32;
    Ok(Some((xorb, index)))
}

fn upload_xorb(client: &reqwest::blocking::Client, base_url: &str, xorb_hex: &str, bytes: Vec<u8>) -> Result<()> {
    let resp = auth(client.post(format!("{base_url}/api/v1/xorbs/default/{xorb_hex}")).body(bytes))
        .send()
        .context("upload xorb")?;
    ensure!(resp.status().is_success(), "xorb upload failed: HTTP {}", resp.status());
    Ok(())
}

/// Fetch a file's reconstruction and reassemble its bytes: for each term, fetch the xorb byte
/// range, deserialize + decompress its chunks, and concatenate.
pub fn reconstruct(base_url: &str, file_hash_hex: &str) -> Result<Vec<u8>> {
    let client = reqwest::blocking::Client::new();
    let resp = auth(client.get(format!("{base_url}/api/v1/reconstructions/{file_hash_hex}")))
        .send()
        .context("query reconstruction")?;
    ensure!(resp.status().is_success(), "reconstruction query failed: HTTP {}", resp.status());
    let recon: serde_json::Value = resp.json().context("parse reconstruction")?;

    let fetch_info = &recon["fetch_info"];
    let mut out = Vec::new();

    for term in recon["terms"].as_array().context("missing terms")? {
        let xorb = term["hash"].as_str().context("term missing hash")?;
        let start = term["range"]["start"].as_u64().context("term missing range.start")? as u32;
        let end = term["range"]["end"].as_u64().context("term missing range.end")? as u32;

        // Find the fetch_info entry covering this term's chunk range.
        let entries = fetch_info[xorb].as_array().context("missing fetch_info for xorb")?;
        let fi = entries
            .iter()
            .find(|e| {
                let s = e["range"]["start"].as_u64().unwrap_or(u64::MAX) as u32;
                let en = e["range"]["end"].as_u64().unwrap_or(0) as u32;
                s <= start && en >= end
            })
            .context("no fetch_info range covers this term")?;

        let url = fi["url"].as_str().context("fetch_info missing url")?;
        let bs = fi["url_range"]["start"].as_u64().context("missing url_range.start")?;
        let be = fi["url_range"]["end"].as_u64().context("missing url_range.end")?; // inclusive

        let body = auth(client.get(url).header("Range", format!("bytes={bs}-{be}")))
            .send()
            .context("fetch xorb range")?
            .bytes()
            .context("read xorb range body")?;

        // Deserialize + decompress exactly the chunks this term spans.
        let mut cursor = Cursor::new(body.as_ref());
        for _ in start..end {
            let (data, _compressed_len, _uncompressed_len) =
                deserialize_chunk(&mut cursor).map_err(|e| anyhow::anyhow!("deserialize chunk: {e}"))?;
            out.extend_from_slice(&data);
        }
    }

    let offset = recon["offset_into_first_range"].as_u64().unwrap_or(0) as usize;
    if offset > 0 && offset <= out.len() {
        out.drain(0..offset);
    }
    Ok(out)
}
