//! # `xet_core` — facade over the vendored xet-core fork
//!
//! The xetd spec (`Prompt.md` §13) refers to the **upstream** `huggingface/xet-core`
//! crate names — `merklehash`, `cas_object`, `mdb_shard`, `chunking`. The vendored
//! fork [`SEBK4C/xet-core`](https://github.com/SEBK4C/xet-core) consolidated those into
//! a few packages, so this crate re-exports the fork's modules under the spec's names.
//!
//! There is **no logic here** — only naming. See `CLAUDE.md` ("Reuse mandate & the
//! xet-core bridge") for the full spec→fork map. Re-export the right thing, never
//! re-derive a wire format.

// --- Core data structures (`xet_core_structures`) ---

/// BLAKE3-keyed hashing, the Merkle aggregated-hash tree, and the byte-swapped
/// hash-string form (spec: `merklehash` / `merkle` / `hashstr`).
pub use xet_core_structures::merklehash;

/// Xorb (de)serialization, the `CasObjectInfo` footer, per-chunk headers, and the
/// `None`/`LZ4`/`ByteGrouping4LZ4` compression schemes (spec: `cas_object`).
pub use xet_core_structures::xorb_object as cas_object;

/// Shard (de)serialization, the footer, CAS info, and term verification
/// (spec: `mdb_shard`).
pub use xet_core_structures::metadata_shard as mdb_shard;

// --- Data pipeline (`xet_data`) ---

/// Gearhash content-defined chunking plus the deduplication helpers
/// (spec: `chunking` + the three-tier dedup of §7). The CDC chunker lives at
/// [`deduplication::chunking`].
pub use xet_data::deduplication;

/// Assemble file bytes from reconstruction terms (spec: §8.2 download path).
pub use xet_data::file_reconstruction;

// --- CAS client (`xet_client`) ---

/// CAS HTTP client + wire types, the hub token client, and the local chunk cache
/// (dedup tier 2). Note `cas_client::simulation::local_server` is a ready-made local
/// CAS server worth studying for `xetd`'s M0 endpoints.
pub use xet_client::{cas_client, cas_types, chunk_cache, hub_client};
