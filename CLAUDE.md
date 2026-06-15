# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Repository status

**M0 skeleton scaffolded** — the workspace compiles, `xetd` runs, and everything is wired to the vendored fork. `Prompt.md` stays the design source of truth (§1–§17 spec + the E2E smoke suite). Layout:

```
Cargo.toml                  # workspace (members: crates/*; excludes vendor/)
crates/
  xet-core/   → lib xet_core   # facade re-exporting the fork (the bridge; see below)
  xetd/       → bin xetd        # server: real CLI + axum router; CAS endpoints are 501 stubs
    tests/{common,conformance,m0_core_cas}   # harness + #[ignore] stubs citing fork paths
  xet-agent/  → lib xet_agent   # ingest/reconstruct stubs (todo!)
  xetfs/      → lib xetfs        # VFS placeholder (no `fuser` until M2)
vendor/xet-core/                # git submodule: SEBK4C/xet-core @ b1374f5 (path-dep'd)
```

**Real vs stub today:** conformance passes (4/4 vectors against the fork). On the server, `POST /xorbs` (the real integrity gate via `XorbObject::validate_xorb_object` + idempotency), `GET /xorb-data` (inclusive ranged serving), and `/admin/test/metric` are implemented over a local-fs `BlobStore` (`crates/xetd/src/{blob,state}.rs`); `GET /reconstructions`, `GET /chunks`, and `POST /shards` still return `501`. `xet-agent`/`xetfs` (ingest/reconstruct/mount) remain `todo!()`. Next M0 step: the reconstruction endpoint + the agent round-trip, then de-stub `m0_core_cas.rs`. Keep this file current as code lands.

## What this builds

A self-hostable storage system that speaks the **Xet protocol** (`XET-BLAKE3-GEARHASH-LZ4` suite) so any conforming Xet client — including Hugging Face's stock `hf-xet` — can upload/download unmodified, plus a POSIX FUSE filesystem whose files are reconstructed on demand from chunk-deduplicated storage. Three artifacts (names are placeholders):

- **`xetd`** — userspace HTTP daemon implementing the CAS API; stateless handlers over two stores.
- **`xet-agent`** — client/ingest library: chunk → hash → 3-tier dedup → xorb/shard (de)serialize → range-fetch → decompress → verify. Either embeds `xet-core`'s pipeline pointed at `xetd`, or calls its format crates directly.
- **`xetfs`** — FUSE mount (`fuser` crate) presenting the VFS; uses `xet-agent` for the data path.

## Architecture (the cross-cutting picture)

**Data-model chain** (read §2, §4, §6 together): a *file* is an ordered list of *terms*; each term is a contiguous *chunk-index range within one xorb*; a *xorb* (≤64 MiB, ≤8192 chunks) aggregates LZ4-compressed *chunks*; chunks come from content-defined (Gearhash) chunking and are addressed by BLAKE3-keyed hashes; *shards* are the binary metadata (file reconstructions + which-chunk-lives-in-which-xorb CAS info). Reconstruction is queried by *file hash* and returns terms + presigned byte-range URLs.

**Two stores inside `xetd`:**
- **Index DB** (SQLite/WAL recommended; DDL in §6.4) — *mutable* metadata: xorb catalog (incl. per-chunk boundary offsets), global chunk index, shards/files, and the **VFS catalog**.
- **`BlobStore`** (§5) — *immutable*, content-addressed xorb bytes. Trait with `local-fs` and `s3`/Ceph-RGW impls; hash-fanned-out keys.

**The bulk-data hot path stays off `xetd`:** reconstruction emits presigned ranged URLs and clients fetch xorb bytes **directly** from the backend (RGW presigned URL, or `xetd`'s own HMAC-signed `xorb-data` endpoint for local-fs). Keep all persistence behind `BlobStore` — no direct filesystem calls in handlers (§10).

**The VFS catalog is this project's own invention, not part of Xet:** a Xet CAS is addressed purely by content hash and has no filenames/dirs/perms/mtimes. The `volumes`/`entries` tables (path → file_hash, mode, mtime, …) bridge "pile of dedup'd blobs" → "mountable tree" (§9.1). Start with mutable rows; the schema leaves room for a later versioned/commit model.

**"Only new data uploads" = client dedup + server idempotency (§7):** the client checks three tiers in order — session set, local shard cache, then the global dedup API (`GET /chunks`) for *eligible* chunks; only all-miss chunks are packed into new xorbs. `POST /xorbs` is idempotent (`was_inserted:false`). This delta-upload property is the core value and exactly what the M1 smoke asserts.

## Reuse mandate & the xet-core bridge (SEBK4C fork)

**Do not re-derive wire formats or constants** — reuse the `xet-core` crates so the build is byte-for-byte conformant. The intended dependency is **[`SEBK4C/xet-core`](https://github.com/SEBK4C/xet-core)**, a fork of `huggingface/xet-core` (workspace `version = 1.5.2`, `edition = 2024` → needs Rust ≥ 1.85).

That fork **consolidated** upstream's many crates into five packages, so the spec's upstream crate names (`merklehash`, `cas_object`, `mdb_shard`, `chunking`) are now **modules inside** these packages, not standalone crates. Map spec → fork before using:

| Spec / upstream name | Fork package | Module path |
|---|---|---|
| `merklehash` (BLAKE3-keyed hash, Merkle root, hash-string) | `xet-core-structures` | `xet_core_structures::merklehash` |
| `cas_object` (xorb (de)serialize, `CasObjectInfo` footer, chunk header, LZ4 / `ByteGrouping4LZ4`) | `xet-core-structures` | `xet_core_structures::xorb_object` |
| `mdb_shard` (shard (de)serialize, footer, CAS info, term verification) | `xet-core-structures` | `xet_core_structures::metadata_shard` |
| Gearhash CDC chunking | `xet-data` | `xet_data::deduplication::chunking` (wraps the external `gearhash` crate) |
| dedup helpers + fragmentation control (§7.3) | `xet-data` | `xet_data::deduplication` (`file_deduplication`, `defrag_prevention`, `data_aggregator`) |
| file reconstruction (terms → bytes) | `xet-data` | `xet_data::file_reconstruction` |
| CAS HTTP client + wire types | `xet-client` | `xet_client::{cas_client, cas_types}` |
| hub token client (`xet-read/write-token`) | `xet-client` | `xet_client::hub_client` |
| local chunk cache (dedup tier 2) | `xet-client` | `xet_client::chunk_cache` |
| async runtime / config / logging | `xet-runtime` | `xet_runtime::*` |

There is **no single `xet_core` facade crate** in the fork, so the spec's conformance stubs (`xet_core::{chunk, merkle, hashstr}`, `xet_core::cas_object`, `xet_core::mdb_shard`) must be rewritten against the module paths above — or wrapped in a thin local facade crate that re-exports them under those names.

**High-value reuse — the fork already ships a local CAS server.** `xet_client::cas_client::simulation::local_server` (`server.rs`, `handlers.rs`) implements CAS endpoints, and `simulation_handlers.rs` / `simulation_control_client.rs` are a test control surface much like the spec's `--test-hooks`. M0's `xetd` should start from / closely study this rather than implementing the four endpoints cold; `local_client.rs` / `memory_client.rs` / `direct_access_client.rs` are reference CAS backends.

Hand-rolling the 256-entry Gearhash table, the BLAKE3 domain keys, the byte-swapped hash-string form, or the Merkle cut-point rule will silently break interop. Suite constants are in §2.1; verify against the current protocol spec before shipping.

## Easy-to-get-wrong invariants

- **`range` vs `url_range`:** chunk-index ranges are end-**exclusive** `[start,end)`; `url_range` is an HTTP byte range and end-**inclusive**. Never conflate them (§4.2, §6.2).
- **Global-dedup eligibility:** a chunk is indexed iff it is the file's first chunk OR `u64_le(last 8 bytes of its hash) % 1024 == 0`. The server's global index is populated *exactly* from shard chunks with bit 31 (`GLOBAL_DEDUP_ELIGIBLE`) set (§2.2, §6.3).
- **Ordering invariant:** every xorb referenced by a shard must already be uploaded before that shard (`POST /shards` returns 400 otherwise) (§4.5, §8.1).
- **Xorb integrity gate:** on upload, recompute the Merkle root over the chunk hashes and reject (400) unless it equals the claimed `{xorb_hash}` (§4.4).
- **Shards: uploaded without footer, stored with footer** (synthesize and append on store) (§2, §4.5).
- **Presigned-URL ↔ cache alignment:** reconstruction responses and any `Cache-Control: max-age` must not outlive the presigned URL TTL (§5.4, §10).

## Milestones & CI gating

Build in order M0→M5; each CI job gates the next, preceded by a `conformance` gate (test vectors). Deliverables per stage are in §15.

- **conformance** — hash/string/merkle/verification vectors; the build is wire-incompatible if this fails.
- **M0** Core CAS (local-fs) · **M1** Dedup · **M2** Read-only VFS · **M3** Writable VFS · **M4** Ceph/S3 · **M5** Operate (GC/scrub/tokens/metrics).
- **Decisive probe (M3):** an in-place mid-file edit through the mount must yield the *same `file_hash`* as a from-scratch ingest of the resulting bytes (`incremental == full`). That single assertion validates the entire write-back / re-chunk / Merkle-splice chain.

## Commands

The workspace builds today; test assertions are `#[ignore]` stubs to fill in per milestone. The harness in `crates/xetd/tests/common/mod.rs` spawns the real `xetd` binary via `CARGO_BIN_EXE_xetd`.

```
git submodule update --init           # after a fresh clone (or clone with --recurse-submodules)
cargo build                           # first build also compiles the vendored fork
cargo run -p xetd -- --help           # the server CLI
cargo test --no-run --workspace       # compile every target incl. test binaries

cargo test --test conformance         # protocol vectors — gate (5 ignored stubs today)
cargo test --test m0_core_cas         # M0 round-trip + 4 wire contracts (ignored stub)
cargo test -- --ignored               # run the stubs (they todo!/panic until implemented)
```

Later stages add (per `Prompt.md`): `m1_dedup`; `m2_ro_vfs` (needs `/dev/fuse` + `CAP_SYS_ADMIN`); `m3_writable -- --test-threads=1` (FUSE mounts: serialize); `m4_s3_rgw` (Docker / testcontainers; `XET_S3_IMAGE=quay.io/ceph/demo … -- --ignored` for the real-RGW lane); `m5_operate`.

**`xetd` CLI:** `--listen <addr>` `--data-dir` `--db` `--durability {close|fsync}` `--auth {loopback|tokens}` `--backend {local-fs|s3}` (`--blob-root` | `--s3-endpoint`/`--s3-bucket`/`--s3-path-style`) `--ready-file` `--test-hooks`.

**Test-only control surface** (behind `--test-hooks`, never in prod): `/admin/test/{metric,fault,crash_at,dedup_key,gc,scrub}` drives metrics and fault/crash injection for the smokes.

**Deterministic test data:** generate bytes with the harness's splitmix64 `gen_blob(seed,len)` — never an external RNG, so CDC chunk boundaries stay stable across runs.

## Recommended stack (§13)

Rust; `axum` for HTTP; `rusqlite`/`sqlx` for SQLite (RocksDB if write-heavy; Postgres only for multi-replica); `aws-sdk-s3` for Ceph RGW; `fuser` for FUSE; `blake3` + `lz4_flex` via `xet_core_structures::xorb_object`. (The fork already pins `axum 0.8`, `reqwest 0.13`, `blake3 1.8`, `lz4_flex 0.13`, `gearhash 0.1`.)
