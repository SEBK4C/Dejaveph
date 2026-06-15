# Dev Log

Append-only engineering journal for the `/loop`-driven hardening + improvement cycles.
Newest entry on top. Each iteration: build → fix a vuln → test → form a new hypothesis →
patch+test it → push. Isolation, deployment, and QoL notes accumulate here.

---

## Iteration 5 — 2026-06-15 ~13:10 UTC — blob write-path concurrency (branch `harden/security-iter5`, stacked on iter4)

**Angle this round:** concurrency/durability of the local-fs blob write path (the HIGH+MEDIUM
audit backlog is cleared; moving to the queued future angles).

**Fix — concurrent `put` of the same novel xorb could persist a corrupt object + double-count
(MEDIUM).** `LocalFsBlobStore::put` used a fixed temp name `.{hash}.tmp` and published with
`rename`. Two simultaneous uploads of the same novel hash therefore:
1. **Corruption:** shared the one temp file — the second writer's `O_TRUNC` open could truncate
   the first writer's bytes mid-publish, so a partial/corrupt object could land under the content
   hash (the in-memory integrity gate passed; the *stored* file didn't).
2. **Double-count:** `rename` always replaces, so every racing writer returned `Ok(true)` →
   `xorb_puts`/`novel_bytes` over-counted.

Fix: **unique temp name** per write (`.{hash}.{pid}.{seq}.tmp`, process-wide `AtomicU64`) so
writers never share a temp; **publish via `hard_link`** — unlike `rename` it returns
`AlreadyExists` when the final exists, so a racer is correctly reported `false` and each object is
counted exactly once. Temp is removed best-effort afterward.

**New test `m0_concurrent_put`:** 24 simultaneous identical uploads → exactly one
`was_inserted:true`, `xorb_puts == 1`, and the served bytes are byte-exact (not truncated). Fails
on the old `rename` path (all racers report inserted). Full sweep green incl. FUSE.

---

## Iteration 4 — 2026-06-15 ~12:55 UTC — capability-URL access control (branch `harden/security-iter4`, stacked on iter3)

**Angle this round:** capability-based access control (vs auth/panic, resource, integrity).

**Fix — local-fs `/xorb-data` was unsigned + non-expiring though docs claimed "HMAC-signed"
(MEDIUM; deferred from iter2/iter3).** Implemented the real capability URL:
- New `cap` module: a BLAKE3 keyed-hash MAC (HMAC-equivalent, key already in-tree) over
  `(hash, exp)`. `presign_get` now returns `…/xorb-data/{hash}?exp=<unix>&sig=<mac>` with the
  900 s TTL it was already passed. Per-process CSPRNG key (`getrandom`, fail-closed).
- Access model = **capability OR bearer** (tokens mode; loopback stays open). `auth_mw` exempts
  `/xorb-data` from the bearer requirement; the handler accepts a valid signed+unexpired
  capability (bulk path needs no bearer, per §10) OR a valid read/write bearer. The additive
  choice keeps **loopback `m0_xorbs`** and **bearer `m5_operate`** green while adding bearer-less
  signed fetches.

**Why feature-breaking → branch:** in tokens mode `/xorb-data` now rejects unsigned+unauth
requests (was bearer-only). Resolves the §5.4/§10 doc-vs-code gap — the CLAUDE.md claim
"HMAC-signed xorb-data endpoint for local-fs" is now true.

**Tests:** `cap` unit tests (sign/verify, expiry, tamper, wrong-key/hash) + new integration
`m4_capability` (signed URL serves bearer-less → 206; unsigned+no-bearer → 403; bearer → 206;
tampered sig → 403). Full sweep green incl. FUSE; no stale mounts.

---

## Iteration 3 — 2026-06-15 ~12:40 UTC — content-integrity + deploy templates (branch `harden/security-iter3`, stacked on iter2)

**Angle this round:** content-addressing integrity (vs iter1 auth/panic, iter2 resource/concurrency).

**Fix — `register_file` didn't verify `file_hash` (MEDIUM, content/cache poisoning).** A
write-token holder could register `file_hash X → arbitrary terms (content Y)`; any reader of X
got Y. The fix recomputes the Merkle file hash server-side from the chunks the terms actually
reference and rejects a mismatch (`400`):
- `XorbMeta` now stores per-xorb `chunk_hashes` (from the validated footer) + an `unpacked_len(i)`
  helper (cumulative offsets → per-chunk length).
- `register_file` walks the terms, builds the ordered `(chunk_hash, unpacked_len)` sequence, and
  asserts `file_hash(seq) == claimed` before inserting.

**Why this is safe / correctly scoped:** the agent computes the same file hash over the same
chunks, and dedup guarantees referenced xorbs carry identical chunk hashes — so legitimate
registrations are unaffected. The *entire existing e2e suite is the correctness regression*:
m0_core_cas, m1_dedup, and especially **m3_writable** (re-ingest → re-register through the agent,
the `incremental == full` path) all pass unchanged. The iter1 `m0_register_validation` test
registered a bogus hash with a valid range expecting 200 — now correctly 400 (updated).

**New test `m0_filehash_integrity`** (the actual poisoning attempt): bind A's file_hash to B's
content → 400; single-bit-flipped hash over valid terms → 400; correct hashes → 200; original
file still reconstructs. **Feature-affecting** (registration now enforces a contract it didn't),
so branched + PR'd.

**QoL — three-machine flake templates** (task: "define template for three machines" +
plug-and-play Ceph). Added `templates/{gateway,client,demo}` so `nix flake init -t …#gateway`
yields a ready NixOS config. (On the deployment branch family; see commit.)

**Tests:** new integrity test + full sweep (conformance, m0/m1, m2/m3 FUSE, m5) green; no stale
mounts.

---

## Iteration 2 — 2026-06-15 ~12:25 UTC — resource & concurrency hardening (branch `harden/security-iter2`, stacked on iter1)

**Angle this round:** resource-exhaustion + concurrency/data-loss (vs iter1's auth/panic angle).

**Fix — `get_range` memory amplification (LOW→ real DoS lever).** local-fs `get_range` did
`tokio::fs::read(whole_file)` then sliced, so *every* ranged GET pulled the full ≤64 MiB xorb
into memory. Many small concurrent ranges = large memory/IO amplification. Rewrote to
`File::open` + `seek(start)` + `read_exact(len)` — reads only the requested span; also removed
the `end as usize + 1` overflow path. New test `m0_range_edges` asserts byte-exact slices at
last-byte / single-byte / open-ended / past-EOF-clamp / unsatisfiable boundaries.

**Fix — GC TOCTOU data-loss race (MEDIUM, test-hook).** `test_gc` computed the root set under
one lock, released it, then deleted + re-locked per xorb — so a `register_file` landing in the
window could reference a xorb about to be swept, orphaning a live file. Rewrote so the root set
is computed AND the unreferenced xorbs are evicted from the index under a **single** lock; since
`register_file` rejects terms whose xorb isn't in the index, eviction-under-lock means no
concurrent register can reference a doomed xorb. Async blob deletes run after, on a consistent
index.

**Deferred (needs care, not rushed):** HMAC+TTL signing for the local-fs presign URL. Doing it
right means making `/xorb-data` a capability URL (exempt from bearer, gated by signature+expiry)
— an auth-model change that reworks `auth_mw` + the m0_xorbs direct-access test. Scheduled as
its own iteration rather than a rushed half-measure.

**Tests:** new `m0_range_edges` + full sweep — conformance 4/4, m0/m1, m2/m3 FUSE, m5
(gc/scrub) all green; no stale mounts.

---

## Iteration 1 — 2026-06-15 ~11:55 UTC — security hardening (branch `harden/security-iter1`)

**Context.** Prior manual audit of `main` (`0b289e2`) surfaced 2 HIGH, 3 MEDIUM, 3 LOW
findings across `xetd`. This iteration patches the two HIGH items with regression tests.

**Isolation decision (the "VM" requirement).** The integration harness
(`crates/xetd/tests/common/mod.rs`) already spawns `xetd` on `127.0.0.1:0` (ephemeral port)
against temp data dirs — it cannot reach this Proxmox host's real services. The only
host-affecting targets are the FUSE mounts (`m2_ro_vfs`, `m3_writable`), which need
`/dev/fuse` + `CAP_SYS_ADMIN`. Strategy:
- **Code isolation:** all work on a git branch, PR'd before merge to `main`.
- **Runtime isolation:** non-privileged tests (conformance, m0, m1, m5) run directly —
  they bind localhost ephemeral ports and self-clean. FUSE tests gated/containerized.
- **Deferred:** a proper qemu/Nix-VM test harness is tracked as a QoL deliverable
  (see "Backlog"), not run per-iteration — a full VM boot per 30-min tick is too heavy.

**HIGH-1 — reconstruct OOB panic under held Mutex → permanent server DoS.**
`register_file` stored term `start`/`end` without bounding them to the xorb's chunk count;
`reconstruct` then indexed `boundary_offsets[end-1]` *inside* the `index.lock()` scope, so an
out-of-range (or `end==0` underflow) term panicked while holding the `std::sync::Mutex`,
poisoning it — every later `.lock().unwrap()` then panics. One crafted request bricks the
server. Fix: validate `0 < start < end <= num_chunks` at registration; defensive `.get()` in
reconstruct. (Mutex-poison resistance via `parking_lot` tracked separately to keep this diff
reviewable.)

**HIGH-2 — predictable auth token on RNG failure.** `random_token` left the buffer all-zeros
if `/dev/urandom` open/read failed (error discarded with `let _ =`), yielding a guessable
`write-000…0` token in `--auth tokens` mode. Fix: source from `getrandom`/OsRng and abort on
failure rather than serve a degraded token.

**Tests.** Added regression coverage asserting malformed term ranges are rejected (`400`) and
never reach the panicking path.

**Second angle (same iteration) — footer-offset poison via `POST /xorbs`.**
Hypothesis: a validly-rooted xorb could carry inconsistent footer arrays (short or
non-monotonic `unpacked_chunk_offsets`) that pass the integrity gate but then underflow/OOB in
`Index::index_chunks` — which runs *under the index lock*, so same poison-DoS class via a
different entry point. **Verified NEGATIVE:** the fork's `XorbObject::deserialize` rejects any
`boundaries_version != 1` (xorb_object_format.rs:581,666) and enforces all three footer arrays
have length `== num_chunks` (589–612); `validate_xorb_object` checks `unpacked_chunk_offsets`
against the actual cumulative decompressed sizes (1136–1141), guaranteeing monotonicity. So
`end - prev_end` can't underflow and `[i]` can't OOB. The gate already defends this — no patch
needed, documented as a confirmed-safe path.

But the investigation reframed the real risk: **the amplifier is `std::sync::Mutex` poisoning**
— *any* panic under the lock (the two found, plus any future one) bricks the whole server.
Patched the class, not just the instances:
- **Poison-proof locking:** `Index` mutex → `parking_lot::Mutex`; a panic while holding it
  releases on unwind instead of poisoning. One panicking request can no longer brick the rest.
- **Constant-time token compare:** `auth_mw` now uses `subtle::ConstantTimeEq` (no
  data-dependent branch on the secret), closing a timing side-channel (was MEDIUM in iter-1).

Tests re-run after both changes: m0_register_validation, m5_operate (token path), m0/m1, and
FUSE m3 all green.

### Backlog / hypotheses for later iterations
- [x] MEDIUM: `register_file` does not verify `file_hash` commits to the terms' content — iter3
      (content-addressing bypass / cache poisoning). Closes with the binary `mdb_shard` path.
- [ ] MEDIUM: local-fs `presign_get` returns an unsigned, non-expiring URL though docs claim
      "HMAC-signed". Implement HMAC+TTL or correct the design docs.
- [ ] MEDIUM: non-constant-time bearer token comparison (`subtle::ConstantTimeEq`).
- [x] LOW: `get_range` reads the whole object into memory per request (IO amplification). — iter2
- [x] LOW: GC TOCTOU data-loss race (test-hooks only). — iter2
- [x] MEDIUM: local-fs presign HMAC+TTL capability URL — iter4 (capability OR bearer).
- [ ] QoL: qemu/Nix-VM e2e harness; NixOS module with 1Password secret integration; Ceph
      plug-and-play deployment doc.
