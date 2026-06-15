# Dev Log

Append-only engineering journal for the `/loop`-driven hardening + improvement cycles.
Newest entry on top. Each iteration: build → fix a vuln → test → form a new hypothesis →
patch+test it → push. Isolation, deployment, and QoL notes accumulate here.

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

### Backlog / hypotheses for later iterations
- [ ] MEDIUM: `register_file` does not verify `file_hash` commits to the terms' content
      (content-addressing bypass / cache poisoning). Closes with the binary `mdb_shard` path.
- [ ] MEDIUM: local-fs `presign_get` returns an unsigned, non-expiring URL though docs claim
      "HMAC-signed". Implement HMAC+TTL or correct the design docs.
- [ ] MEDIUM: non-constant-time bearer token comparison (`subtle::ConstantTimeEq`).
- [ ] LOW: `get_range` reads the whole object into memory per request (IO amplification).
- [ ] LOW: GC TOCTOU data-loss race (test-hooks only).
- [ ] QoL: qemu/Nix-VM e2e harness; NixOS module with 1Password secret integration; Ceph
      plug-and-play deployment doc.
