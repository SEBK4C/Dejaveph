//! `xet-agent` — the client/ingest pipeline (`Prompt.md` §7, §8).
//!
//! M0 implements a working ingest→reconstruct round-trip using the real Xet primitives
//! (Gearhash CDC, BLAKE3 hashing, xorb serialization, file hash) from the vendored fork.
//! File registration uses the server's M0-internal `POST /files` JSON; the binary
//! `mdb_shard` wire format (stock-client interop) is a later refinement. Three-tier dedup
//! (§7.1) also lands later — M0 always packs novel chunks.

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

/// Outcome of an ingest — carries the file's XET-string content hash.
pub struct Ingested {
    file_hash_hex: String,
}

impl Ingested {
    pub fn file_hash_hex(&self) -> String {
        self.file_hash_hex.clone()
    }
}

/// Chunk, pack into xorbs, upload novel xorbs, then register the file's reconstruction terms.
/// Returns the file's content hash (deterministic for identical bytes).
pub fn ingest_bytes(
    base_url: &str,
    volume: &str,
    dest: &str,
    bytes: &[u8],
    _scratch: &Path,
) -> Result<Ingested> {
    // 1. Content-defined chunking (Gearhash CDC); each Chunk carries its BLAKE3 hash + data.
    let chunks = Chunker::default().next_block(bytes, true);
    ensure!(!chunks.is_empty() || bytes.is_empty(), "chunker produced no chunks for non-empty input");

    // 2. Whole-file hash over the ordered (chunk_hash, uncompressed_size) list.
    let file_pairs: Vec<_> = chunks.iter().map(|c| (c.hash, c.data.len() as u64)).collect();
    let fh = file_hash(&file_pairs);

    // 3. Pack chunks into xorbs, upload each, and accumulate one term per xorb.
    let client = reqwest::blocking::Client::new();
    let mut terms = Vec::new();
    let mut i = 0;
    while i < chunks.len() {
        let mut j = i;
        let mut acc = 0usize;
        while j < chunks.len() && (j - i) < MAX_XORB_CHUNKS && acc + chunks[j].data.len() <= MAX_XORB_UNCOMPRESSED {
            acc += chunks[j].data.len();
            j += 1;
        }
        if j == i {
            j = i + 1; // a single chunk larger than the cap still gets its own xorb
        }
        let group = &chunks[i..j];

        let raw = RawXorbData::from_chunks(group, vec![0]);
        let serialized = SerializedXorbObject::from_xorb(raw, /*footer=*/ true, "lz4", 0)
            .context("serialize xorb")?;
        let xorb_hex = serialized.hash.hex();

        let resp = client
            .post(format!("{base_url}/api/v1/xorbs/default/{xorb_hex}"))
            .body(serialized.serialized_data)
            .send()
            .context("upload xorb")?;
        ensure!(resp.status().is_success(), "xorb upload failed: HTTP {}", resp.status());

        let unpacked: u64 = group.iter().map(|c| c.data.len() as u64).sum();
        terms.push(json!({
            "xorb": xorb_hex,
            "start": 0,
            "end": group.len(),
            "unpacked_length": unpacked,
        }));
        i = j;
    }

    // 4. Register the file (terms + VFS catalog entry).
    let total_size: u64 = chunks.iter().map(|c| c.data.len() as u64).sum();
    let resp = client
        .post(format!("{base_url}/api/v1/files"))
        .json(&json!({
            "file_hash": fh.hex(),
            "total_size": total_size,
            "terms": terms,
            "volume": volume,
            "path": dest,
        }))
        .send()
        .context("register file")?;
    ensure!(resp.status().is_success(), "file registration failed: HTTP {}", resp.status());

    Ok(Ingested { file_hash_hex: fh.hex() })
}

/// Fetch a file's reconstruction and reassemble its bytes: for each term, fetch the xorb byte
/// range, deserialize + decompress its chunks, and concatenate.
pub fn reconstruct(base_url: &str, file_hash_hex: &str) -> Result<Vec<u8>> {
    let client = reqwest::blocking::Client::new();
    let resp = client
        .get(format!("{base_url}/api/v1/reconstructions/{file_hash_hex}"))
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

        let body = client
            .get(url)
            .header("Range", format!("bytes={bs}-{be}"))
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

    // Whole-file reconstruction starts at the first term's first chunk.
    let offset = recon["offset_into_first_range"].as_u64().unwrap_or(0) as usize;
    if offset > 0 && offset <= out.len() {
        out.drain(0..offset);
    }
    Ok(out)
}
