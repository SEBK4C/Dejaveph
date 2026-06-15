//! Server state: the immutable `BlobStore` plus the mutable in-memory index
//! (xorb catalog, file reconstructions, VFS catalog) and metrics.
//!
//! M0 keeps the index in memory behind a `Mutex`; SQLite (`Prompt.md` §6) lands later.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};

use xet_core::cas_object::XorbObjectInfoV1;
use xet_core::merklehash::MerkleHash;

use crate::blob::BlobStore;

/// Counters surfaced via `/admin/test/metric/{name}` (test-hooks only).
#[derive(Default)]
pub struct Metrics {
    pub xorb_puts: AtomicU64,
    pub novel_bytes: AtomicU64,
    pub chunk_query_hit: AtomicU64,
    pub chunk_query_miss: AtomicU64,
}

impl Metrics {
    pub fn get(&self, name: &str) -> u64 {
        match name {
            "xorb_puts" => self.xorb_puts.load(Relaxed),
            "novel_bytes" => self.novel_bytes.load(Relaxed),
            "chunk_query_hit" => self.chunk_query_hit.load(Relaxed),
            "chunk_query_miss" => self.chunk_query_miss.load(Relaxed),
            _ => 0,
        }
    }
}

/// Per-xorb index entry: the compressed-boundary offsets (chunk range → byte range) captured
/// from the validated footer at upload time (`Prompt.md` §6.2).
pub struct XorbMeta {
    #[allow(dead_code)]
    pub num_chunks: u32,
    /// Compressed end offset of each chunk within the serialized xorb.
    pub boundary_offsets: Vec<u32>,
    /// Uncompressed cumulative end offset of each chunk.
    #[allow(dead_code)]
    pub unpacked_offsets: Vec<u32>,
}

impl XorbMeta {
    pub fn from_info(info: &XorbObjectInfoV1) -> Self {
        Self {
            num_chunks: info.num_chunks,
            boundary_offsets: info.chunk_boundary_offsets.clone(),
            unpacked_offsets: info.unpacked_chunk_offsets.clone(),
        }
    }
}

/// One reconstruction term: a contiguous chunk-index range `[start, end)` within one xorb.
pub struct Term {
    pub xorb: MerkleHash,
    pub start: u32,
    pub end: u32,
    pub unpacked_length: u64,
}

/// A registered file: its total size and ordered reconstruction terms.
pub struct FileRecord {
    #[allow(dead_code)]
    pub total_size: u64,
    pub terms: Vec<Term>,
}

/// Where a chunk lives, for the global dedup index (`Prompt.md` §6.3).
pub struct ChunkLoc {
    pub xorb: MerkleHash,
    pub index: u32,
    pub unpacked_len: u32,
}

/// Mutable metadata. Replaced by the SQLite index store in a later milestone.
#[derive(Default)]
pub struct Index {
    pub xorbs: HashMap<MerkleHash, XorbMeta>,
    pub files: HashMap<MerkleHash, FileRecord>,
    /// VFS catalog: (volume, path) → file_hash (`Prompt.md` §9.1).
    pub catalog: HashMap<(String, String), MerkleHash>,
    /// Global dedup index: chunk_hash → location (`Prompt.md` §6.3).
    pub chunk_index: HashMap<MerkleHash, ChunkLoc>,
}

impl Index {
    pub fn put_xorb(&mut self, hash: MerkleHash, info: &XorbObjectInfoV1) {
        self.xorbs.entry(hash).or_insert_with(|| XorbMeta::from_info(info));
    }

    /// Index every chunk of a freshly stored xorb into the global dedup index.
    /// (M0/M1 index all chunks; §2.2 global-eligibility filtering arrives with the keyed-shard
    /// dedup protocol.) The uncompressed offsets are cumulative end positions, so chunk `i`'s
    /// length is `offset[i] - offset[i-1]`.
    pub fn index_chunks(&mut self, xorb: MerkleHash, info: &XorbObjectInfoV1) {
        let mut prev_end = 0u32;
        for (i, h) in info.chunk_hashes.iter().enumerate() {
            let end = info.unpacked_chunk_offsets[i];
            let unpacked_len = end - prev_end;
            prev_end = end;
            self.chunk_index
                .entry(*h)
                .or_insert(ChunkLoc { xorb, index: i as u32, unpacked_len });
        }
    }
}

pub struct AppState {
    pub blob: Arc<dyn BlobStore>,
    pub index: Mutex<Index>,
    pub metrics: Metrics,
    /// This server's externally reachable base URL (e.g. `http://127.0.0.1:8080`), used to build
    /// `fetch_info` URLs in reconstruction responses.
    pub base_url: String,
}

impl AppState {
    pub fn new(blob: Arc<dyn BlobStore>, base_url: String) -> Arc<Self> {
        Arc::new(Self {
            blob,
            index: Mutex::new(Index::default()),
            metrics: Metrics::default(),
            base_url,
        })
    }
}
