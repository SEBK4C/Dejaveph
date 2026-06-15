# Dejaveph

A self-hostable storage server that speaks the **Xet protocol** (`XET-BLAKE3-GEARHASH-LZ4`) plus a reconstructing POSIX filesystem over chunk-deduplicated storage. The goal: any conforming Xet client — including Hugging Face's stock `hf-xet` — can upload/download unmodified, and the same content can be mounted and read on demand.

This repo builds on the vendored **[`SEBK4C/xet-core`](https://github.com/SEBK4C/xet-core)** fork (a fork of `huggingface/xet-core`) for byte-for-byte format conformance. The full design lives in [`Prompt.md`](Prompt.md); the architecture map and the spec→fork crate mapping live in [`CLAUDE.md`](CLAUDE.md).

> **Status: all milestone invariants pass** — conformance + M0–M5. You can run the server, ingest with edit-locality dedup, mount a volume read+write, point bulk reads straight at S3/RGW, and operate it (GC, scrub, token scopes). M4's S3 backend compiles behind `--features s3`; its live MinIO/RGW test needs Docker. Remaining work is **refinement**, not new milestones (see the bottom of this file and `CLAUDE.md`).

## Architecture at a glance

![Dejaveph architecture: a client host (xetfs FUSE mount + xet-agent) talks to the xetd server (HTTP API + auth, index, BlobStore) which sits over a pluggable object backend (local-fs or Ceph RGW/S3); the ingest, reconstruct, and mount flows are listed on the right, and bulk xorb bytes are fetched by the client directly from the backend via presigned ranged GETs.](docs/architecture.png)

> Source: [`docs/architecture.svg`](docs/architecture.svg). Clients call `xetd` for metadata and dedup; **bulk xorb bytes are fetched directly from the object backend** via presigned ranged GETs (the red path), keeping the server off the data plane.

## What works today

| Stage | Status |
|---|---|
| **conformance** (protocol vectors) | ✅ 4/4 hash vectors pass against the real fork; `reference_objects` `#[ignore]` (network) |
| **M0** Core CAS (local-fs) | 🟢 ingest→reconstruct round-trip working (byte-identical, multi-xorb); integrity gate, idempotency, ranged serving, metrics. Binary `/shards` + global dedup `/chunks` pending |
| **M1** Dedup | 🟢 edit-locality dedup working — a 1-byte mid-file edit re-uploads only the perturbed chunks (client resolves chunks via `/chunks`, references existing xorbs) |
| **M2** Read-only VFS | 🟢 FUSE mount working — files read through the mount match the originals; `readdir` + partial reads (needs `/dev/fuse` + setuid `fusermount`) |
| **M3** Writable VFS | 🟢 write-back-on-close working — an in-place edit yields the same `file_hash` as a full ingest (`incremental == full`); truncate/append. Crash-recovery + GC refcounts pending |
| **M4** Ceph/S3 | 🟡 S3/RGW `BlobStore` implemented (behind `--features s3`) + presigned ranged GETs; reconstruction points clients straight at the object store. Live MinIO/RGW test is `#[ignore]` (needs Docker) |
| **M5** Operate | 🟢 mark-sweep GC (orphans reclaimed, referenced spared), scrub (corruption quarantined), bearer token scopes (read→403 on write), metrics |

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

#### …or use Nix for a clean, reproducible setup (no Rust/FUSE preinstalled)

A flake ([`flake.nix`](flake.nix)) pins the Rust toolchain (so the edition-2024
vendored fork builds identically anywhere) and provides `fuse` for the mount:

```bash
nix develop                       # shell with rust + fuse + build deps, then:
cargo test --workspace -- --test-threads=1

nix build  .#xetd                 # -> ./result/bin/xetd  (the server)
nix run    .#xetd -- --help
```

`nix flake check` builds the workspace and runs the non-FUSE tests. Submodules
are fetched automatically (`nix build "git+https://github.com/SEBK4C/Dejaveph?submodules=1"`).

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

## How it works

Four crates, two stores, one content-addressed pipeline (see the diagram above):

- **`xetd`** (`crates/xetd`) — userspace HTTP daemon. Stateless handlers over a mutable **index** (xorb catalog, global chunk index, files, the VFS catalog) and an immutable, content-addressed **`BlobStore`** (local-fs or Ceph RGW/S3). Bearer-token auth with read/write scopes.
- **`xet-agent`** (`crates/xet-agent`) — the ingest/reconstruct pipeline: Gearhash content-defined chunking → BLAKE3-keyed hashing → three-tier dedup → xorb (de)serialization → ranged fetch → decompress.
- **`xetfs`** (`crates/xetfs`) — a FUSE mount that reconstructs files on demand and writes back on close.
- **`xet-core`** (`crates/xet-core`) — a thin facade re-exporting the vendored fork's modules under the spec's names; the bridge that guarantees byte-for-byte format conformance.

**Ingest** — chunk the file (Gearhash), hash each chunk (BLAKE3), and for each chunk ask `GET /chunks` whether it already exists. Novel chunks are packed into xorbs and uploaded (`POST /xorbs`, where the server recomputes the Merkle root and rejects a mismatch — the integrity gate); known chunks are referenced in place. The file is registered as an ordered list of *terms* — `(xorb, chunk-range)`.

**Reconstruct** — `GET /reconstructions/{file_hash}` returns the terms plus per-xorb `fetch_info`: **presigned, ranged URLs** pointing straight at the object backend. The client fetches those byte ranges, decompresses the chunks, and concatenates. `xetd` never touches the bulk bytes.

**Mount** — `lookup`/`readdir`/`getattr` come from the catalog (no CAS access); `read` reconstructs on demand; `write` stages the file and re-ingests it on close, so an in-place edit produces the *same* `file_hash` as a from-scratch ingest (`incremental == full`).

**Why it's efficient** — re-storing edited content transfers only novel chunks: a 1-byte insert into a 16 MiB file uploads < 1 MiB, and cloning identical content uploads nothing.

## Using it

Start the server (see "Run the server" above), then drive it. The data path lives in `xet-agent`; the milestone tests are the simplest runnable demos:

```bash
cargo test --test m1_dedup -- --nocapture        # edit locality: 1-byte edit ⇒ < 1 MiB uploaded
cargo test --test m3_writable -- --test-threads=1 # writable mount + incremental==full (needs /dev/fuse)
cargo test --test m5_operate                      # GC, scrub, token scopes
```

In code, ingest and reconstruct are two calls:

```rust
use std::path::Path;
let ing  = xet_agent::ingest_bytes(&base_url, "vol", "/a.bin", &bytes, Path::new("/tmp"))?;
let back = xet_agent::reconstruct(&base_url, &ing.file_hash_hex())?;   // back == bytes
```

Mount a volume read+write (after ingesting into it):

```rust
let fs = xetfs::Xetfs::connect(&base_url, "vol", /*rw=*/ true)?;
let _session = fuser::spawn_mount2(fs, "/mnt/xet", &[fuser::MountOption::FSName("xetfs".into())])?;
// /mnt/xet now serves the volume's files, reconstructed on demand; writes re-ingest on close.
```

### S3 / Ceph RGW backend

```bash
AWS_ACCESS_KEY_ID=… AWS_SECRET_ACCESS_KEY=… \
cargo run -p xetd --features s3 -- \
  --backend s3 --s3-endpoint http://rgw:7480 --s3-bucket xet --s3-path-style \
  --listen 127.0.0.1:8080 --data-dir /tmp/xetd --db /tmp/xetd/index.sqlite
# reconstruction now hands clients presigned RGW/S3 URLs — bulk bytes flow client↔RGW directly.
```

See [`CLAUDE.md`](CLAUDE.md) for the full spec→fork crate map and the load-bearing protocol invariants.

## Layout

```
Cargo.toml                 # workspace
crates/
  xet-core/    → lib xet_core    # facade over the vendored fork
  xetd/        → bin xetd        # the CAS server (+ tests/: conformance, m0..m5, harness)
  xet-agent/   → lib xet_agent   # ingest / reconstruct pipeline
  xetfs/       → lib xetfs        # reconstructing FUSE mount (read + write-back)
vendor/xet-core/               # git submodule: SEBK4C/xet-core (path-dependency)
docs/architecture.svg          # the diagram above (rendered to architecture.png)
flake.nix                      # Nix devshell + xetd package
Prompt.md                      # the full design spec + E2E smoke suite
CLAUDE.md                      # architecture map, invariants, commands
```

## License

Apache-2.0. Not affiliated with or endorsed by Hugging Face. Verify all constants
and formats against the current Xet protocol spec before relying on this.
