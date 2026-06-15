# Dejaveph

A self-hostable storage server that speaks the **Xet protocol** (`XET-BLAKE3-GEARHASH-LZ4`) plus a reconstructing POSIX filesystem over chunk-deduplicated storage. The goal: any conforming Xet client — including Hugging Face's stock `hf-xet` — can upload/download unmodified, and the same content can be mounted and read on demand.

This repo builds on the vendored **[`SEBK4C/xet-core`](https://github.com/SEBK4C/xet-core)** fork (a fork of `huggingface/xet-core`) for byte-for-byte format conformance. The full design lives in [`Prompt.md`](Prompt.md); the architecture map and the spec→fork crate mapping live in [`CLAUDE.md`](CLAUDE.md).

> **Status: early implementation.** The protocol **conformance gate passes** (the server's hashing agrees with stock xet-core) and the **Core CAS ingest→reconstruct round-trip works** end-to-end (chunk → xorb → integrity-gated store → reconstruct, byte-identical). Remaining M0 work: the binary `mdb_shard` upload (`/shards`, for stock-client interop) and global dedup (`/chunks`).

## What works today

| Stage | Status |
|---|---|
| **conformance** (protocol vectors) | ✅ 4/4 hash vectors pass against the real fork; `reference_objects` `#[ignore]` (network) |
| **M0** Core CAS (local-fs) | 🟢 ingest→reconstruct round-trip working (byte-identical, multi-xorb); integrity gate, idempotency, ranged serving, metrics. Binary `/shards` + global dedup `/chunks` pending |
| **M1** Dedup · **M2** RO VFS · **M3** Writable VFS · **M4** Ceph/S3 · **M5** Operate | ⏳ planned (see `Prompt.md` §15) |

## Reproduce it yourself

Tested on Linux with **Rust ≥ 1.85** (the vendored fork is edition 2024; CI uses 1.96).

### 1. Clone — including the vendored fork

```bash
git clone --recurse-submodules https://github.com/SEBK4C/Dejaveph
cd Dejaveph
# already cloned without submodules? pull it in:
git submodule update --init
```

### 2. Build

```bash
cargo build           # first build also compiles the vendored xet-core fork
```

### 3. Run the conformance gate (the proof it's wire-compatible)

```bash
cargo test --test conformance
# running 5 tests
# test vec_chunk_hash ... ok
# test vec_hash_string_roundtrip ... ok
# test vec_internal_node ... ok
# test vec_verification_hash ... ok
# test vec_reference_objects ... ignored
```

These assert the `XET-BLAKE3-GEARHASH-LZ4` suite test vectors against the real
fork primitives, so a passing run means this build hashes identically to stock
`hf-xet`.

### 4. Run the server

```bash
mkdir -p /tmp/xetd/blobs
cargo run -p xetd -- \
  --listen 127.0.0.1:8080 \
  --data-dir /tmp/xetd --db /tmp/xetd/index.sqlite \
  --backend local-fs --blob-root /tmp/xetd/blobs \
  --test-hooks --ready-file /tmp/xetd/ready
```

`xetd --help` lists every flag. On startup it atomically writes `http://<addr>`
to `--ready-file` — that's the contract the test harness waits on.

### 5. Smoke-test the running server (in another shell)

```bash
cat /tmp/xetd/ready
# http://127.0.0.1:8080

curl -s http://127.0.0.1:8080/admin/test/metric/xorb_puts
# 0

curl -s -o /dev/null -w '%{http_code}\n' \
  "http://127.0.0.1:8080/api/v1/reconstructions/$(printf 'f%.0s' {1..64})"
# 404   (reconstruction is live; this hash is just unknown)
```

The full ingest→reconstruct round-trip is exercised by `cargo test --test m0_core_cas`
(it drives the real agent: chunk → xorb upload → register → reconstruct → byte-compare).

### Run the whole suite

```bash
cargo test --workspace          # green; milestone assertions are #[ignore] until implemented
cargo test -- --ignored         # runs the stubs (they panic with a TODO until wired)
```

## Architecture (short)

- **`xetd`** (`crates/xetd`) — userspace HTTP daemon. Two stores: a mutable **index** (xorb catalog, global chunk index, shards, the VFS catalog) and an immutable content-addressed **`BlobStore`** (local-fs first, Ceph RGW / S3 later). Bulk xorb bytes are fetched by clients directly from the backend via presigned/ranged URLs — off the server's data path.
- **`xet-agent`** (`crates/xet-agent`) — the ingest/reconstruct pipeline: content-defined chunking → BLAKE3-keyed hashing → three-tier dedup → xorb/shard (de)serialization → ranged fetch → decompress → verify.
- **`xetfs`** (`crates/xetfs`) — a FUSE mount that reconstructs files on demand (lands at M2).
- **`xet-core`** (`crates/xet-core`) — a thin facade re-exporting the vendored fork's modules under the spec's names; the bridge that guarantees format conformance.

See [`CLAUDE.md`](CLAUDE.md) for the full spec→fork crate map and the load-bearing protocol invariants.

## Layout

```
Cargo.toml                 # workspace
crates/
  xet-core/    → lib xet_core    # facade over the vendored fork
  xetd/        → bin xetd        # the CAS server  (+ tests/: conformance, m0, harness)
  xet-agent/   → lib xet_agent   # ingest / reconstruct pipeline
  xetfs/       → lib xetfs        # reconstructing FUSE filesystem (M2)
vendor/xet-core/               # git submodule: SEBK4C/xet-core (path-dependency)
Prompt.md                      # the full design spec + E2E smoke suite
CLAUDE.md                      # architecture map, invariants, commands
```

## License

Apache-2.0. Not affiliated with or endorsed by Hugging Face. Verify all constants
and formats against the current Xet protocol spec before relying on this.
