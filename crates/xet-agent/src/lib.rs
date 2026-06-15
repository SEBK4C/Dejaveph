//! `xet-agent` — the client/ingest pipeline (`Prompt.md` §7, §8).
//!
//! **M0 stub.** This declares the public surface the E2E harness drives; the bodies
//! are unimplemented. Wire ingest to `xet_core::deduplication` (Gearhash CDC + the
//! three dedup tiers) → `POST /xorbs` → `POST /shards`, and reconstruct to
//! `GET /reconstructions` → ranged xorb fetch → decompress → concat
//! (`xet_core::file_reconstruction`).

use std::path::Path;

/// Outcome of an ingest — carries the file's XET-string content hash.
pub struct Ingested {
    file_hash_hex: String,
}

impl Ingested {
    /// The file's content address, as the byte-swapped XET hash string.
    pub fn file_hash_hex(&self) -> String {
        self.file_hash_hex.clone()
    }
}

/// Chunk, deduplicate, and upload `bytes`, register them at `dest` within `volume`'s
/// catalog, and return the resulting file hash. `scratch` is a per-ingest temp dir.
pub fn ingest_bytes(
    _base_url: &str,
    _volume: &str,
    _dest: &str,
    _bytes: &[u8],
    _scratch: &Path,
) -> anyhow::Result<Ingested> {
    // TODO(M0): xet_core::deduplication chunk+dedup -> upload novel xorbs -> POST /shards
    //           -> upsert the VFS catalog entry (volume, dest). Return Ingested.
    anyhow::bail!("xet_agent::ingest_bytes not yet implemented (M0 stub)")
}

/// Fetch the reconstruction for `file_hash_hex` and return the assembled file bytes.
pub fn reconstruct(_base_url: &str, _file_hash_hex: &str) -> anyhow::Result<Vec<u8>> {
    // TODO(M0): GET /reconstructions/{file_hash} -> fetch xorb byte ranges (url_range,
    //           inclusive) -> decompress chunks -> honor offset_into_first_range -> concat.
    anyhow::bail!("xet_agent::reconstruct not yet implemented (M0 stub)")
}
