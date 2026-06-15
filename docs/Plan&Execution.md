# Dejaveph — Plan & Execution

Living tracker for all work: shipped, in-flight, and planned. Update this file as items move.
Companion docs: [`DEVLOG.md`](DEVLOG.md) (per-iteration narrative), [`DEPLOYMENT.md`](DEPLOYMENT.md)
(ops runbook), `../Prompt.md` (spec), `../CLAUDE.md` (architecture + milestone definitions).

**Status legend**
| Mark | Meaning |
|---|---|
| ✅ | Done & merged to `main` |
| 🔵 | Done, on a branch / PR open & unmerged |
| 🟡 | In progress |
| ⬜ | Not started |
| ❄️ | Deferred / blocked (reason noted) |

_Last updated: 2026-06-15 (after loop iteration 5)._

---

## Open PR stack (review state)

Two independent stacks. Review/merge each chain in order.

| PR | Title | Base | Status |
|----|-------|------|--------|
| #1 | harden(sec): bound term ranges + fail-closed token RNG (2 HIGH) | `main` | 🔵 open |
| #3 | harden(sec) iter2: bounded ranged reads + TOCTOU-safe GC | `#1` | 🔵 open (stacked) |
| #5 | harden(sec) iter3: verify file_hash commits to terms | `#3` | 🔵 open (stacked) |
| #7 | harden(sec) iter4: HMAC+TTL capability URL for /xorb-data | `#5` | 🔵 open (stacked) |
| #8 | harden(sec) iter5: unique temp + hard-link publish (concurrent put) | `#7` | 🔵 open (stacked) |
| #2 | feat(nixos): services.xetd + 1Password(opnix)/Ceph | `main` | 🔵 open |
| #4 | feat(xetfs): mount CLI + services.xetfs (client half) | `#2` | 🔵 open (stacked) |
| #6 | feat(templates): three-machine flake templates | `#4` | 🔵 open (stacked) |

**Next merge action (human):** review/merge the security chain `#1 → #3 → #5 → #7 → #8`, then the
deployment chain `#2 → #4 → #6`. After merge, mark the corresponding 🔵 rows below as ✅.
(`scripts/` QoL helpers — `dejaveph-doctor`, `bootstrap-ceph` — are already on `main`: ✅.)

---

## A. Core protocol & milestones

Per `CLAUDE.md`. These are the spec deliverables; all milestone invariants currently pass.

| Item | Status | Notes |
|---|---|---|
| conformance vectors | ✅ | 4/4 (1 network stub `#[ignore]`) |
| M0 Core CAS (local-fs) | ✅ | round-trip byte-identical, multi-xorb |
| M1 Dedup | ✅ | edit-locality: only novel chunks re-upload |
| M2 Read-only VFS (FUSE) | ✅ | inode tree from catalog; reconstruct on read |
| M3 Writable VFS | ✅ | write-back-on-close; `incremental == full` probe passes |
| M4 S3/Ceph-RGW backend | ✅ | `--features s3`, presigned GETs |
| M4 live RGW test (`m4_s3_rgw`) | ❄️ | needs Docker/testcontainers + s3-built binary |
| M5 Operate (GC/scrub/tokens/metrics) | ✅ | mark-sweep GC, scrub, bearer scopes |

### Refinements (post-milestone, from CLAUDE.md)
| Item | Status | Notes |
|---|---|---|
| Binary `mdb_shard` `/shards` (stock-`hf-xet` interop) | ⬜ | replaces M0-internal `POST /files` JSON |
| `register_file` → verify `file_hash` commits to terms | 🔵 | done in PR #5 (see B) |
| M3 crash-recovery + reflink (`copy_file_range`) | ⬜ | |
| Refcount-based online GC | ⬜ | current GC is mark-sweep, test-hook |
| Dedup tiers 1–2 (session + local shard cache) | ⬜ | §7.1 |
| Min-dedup-run fragmentation control (§7.3) | ⬜ | |
| Real JWT issuance | ⬜ | current tokens are opaque per-process |
| SQLite index store (replace in-memory) | ⬜ | §6.4 DDL exists in spec |

---

## B. Security hardening (the audit loop)

Findings from the rolling audit. Severity from the original review of `main@0b289e2`.

| # | Finding | Sev | Status | Where |
|---|---|---|---|---|
| HIGH-1 | reconstruct OOB panic under held Mutex → permanent DoS | HIGH | 🔵 | PR #1 (range bounds + checked indexing) |
| HIGH-2 | predictable all-zero token on RNG failure (auth bypass) | HIGH | 🔵 | PR #1 (getrandom + fail-closed) |
| CLASS | `std::sync::Mutex` poisoning amplifies any panic-under-lock | HIGH | 🔵 | PR #1 (`parking_lot`, no poison) |
| MED | non-constant-time bearer comparison (timing) | MED | 🔵 | PR #1 (`subtle::ConstantTimeEq`) |
| LOW | `get_range` reads whole object per request (mem/IO amp) | LOW | 🔵 | PR #3 (seek + read_exact) |
| MED | GC TOCTOU → can orphan a live file | MED | 🔵 | PR #3 (single-lock root+evict) |
| — | footer-offset poison via `POST /xorbs` (hypothesis) | — | ✅ | Verified **negative** — fork gate already blocks it |
| MED | `register_file` doesn't verify `file_hash` (content poisoning) | MED | 🔵 | PR #5 — server recomputes file hash from terms' chunks, rejects mismatch |
| MED | local-fs presign is unsigned/non-expiring (doc says HMAC) | MED | 🔵 | PR #7 — BLAKE3-keyed MAC + TTL; `/xorb-data` = capability OR bearer |
| MED | concurrent `put` of same novel xorb: corrupt object + double-count | MED | 🔵 | PR #8 — unique temp + hard-link publish |
| LOW | no TLS (cleartext bearer) | LOW | ⬜ | deployment: front with TLS proxy (doc'd in DEPLOYMENT.md) |

### Future audit angles (queued, ~1 per iteration)
- ⬜ Unbounded allocation / decompression bomb on the reconstruct path.
- ⬜ Volume/path catalog: no per-volume auth scoping (tokens are global).
- [x] Idempotency / race in concurrent `put_xorb` — **done iter5 (PR #8)**; GC race done iter2.
- ⬜ Fuzz the xorb footer parser + `parse_range` (cargo-fuzz / arbitrary).

---

## C. Deployment · NixOS · Secrets

| Item | Status | Where |
|---|---|---|
| `services.xetd` NixOS module (hardened systemd) | 🔵 | PR #2 `nixos/module.nix` |
| 1Password (opnix) RGW-secret integration | 🔵 | PR #2 `nixos/example.nix` |
| `packages.xetd-s3` (s3-featured build) | 🔵 | PR #2 `flake.nix` |
| Ceph plug-and-play runbook | 🔵 | PR #2 `docs/DEPLOYMENT.md` |
| `xetfs` mount CLI (`xetfs --server … [--rw] <mnt>`) | 🔵 | PR #4 `crates/xetfs/src/main.rs` |
| `services.xetfs` mount module (+ `XETD_TOKEN` from 1Password) | 🔵 | PR #4 `nixos/xetfs.nix` |
| **Three flake templates** (`nix flake init -t …#{gateway,client,demo}`) | 🔵 | PR #6 — see §E |
| **`dejaveph` helpers** (`doctor`, `bootstrap-ceph`) | ✅ | `scripts/` on `main` — bash, not yet a unified Rust CLI |
| `/healthz` + `--ready` semantics for `systemctl` | ⬜ | "is it working?" in one command |
| opnix template that *creates* the RGW item from `radosgw-admin` | ⬜ | closes the manual copy-paste gap |
| qemu / Nix-VM e2e test harness | ❄️ | backlog; needs `nix` on a builder (this host has none) |

### Bare-minimum deploy (target UX)
- **Truly minimal:** `services.xetd.enable = true;` with `backend = "local-fs"` → no Ceph, no
  secrets. This is the on-ramp; Ceph + 1Password are an *upgrade*, not a prerequisite.
- **Full (NixOS+Ceph+1Password):** 6 steps — 1 RGW user + 1 bucket; 1 vault + 1 item + 1
  service-account token; 2 flake inputs; import 2 modules; ~6-line config; `nixos-rebuild switch`.
  (Detailed in DEPLOYMENT.md.)

---

## D. macOS client ("looks like Dropbox")

Decision: **do NOT port the FUSE mount to macOS** (macFUSE needs a kernel extension — not
plug-and-play). Use Apple's **File Provider framework** (how modern Dropbox/iCloud work: dataless
placeholders, materialize-on-access, Finder badges, no kext). Our on-demand reconstruction maps
1:1 onto it.

| Item | Status | Notes |
|---|---|---|
| Design doc (`docs/macos-client.md`) | ⬜ | write FIRST, review before any Swift |
| `libxetagent` — `xet-agent` exposed via `uniffi` (Rust core, FFI) | ⬜ | reused by macOS + future Windows |
| File Provider Extension (Swift) | ⬜ | the "Dropbox" behavior |
| Menu-bar app (SwiftUI) — status, mounts, sign-in | ⬜ | |
| MVP path: FUSE-T + `xetfs` CLI + tiny tray app | ⬜ | days, not weeks; not polished |
| 1Password (macOS) for the bearer token | ⬜ | mirror the NixOS opnix pattern |

**Effort:** MVP (FUSE-T wrapper) ≈ days. Real (File Provider + uniffi core) ≈ weeks. The hard
part — chunk reconstruct/ingest — already exists in `xet-agent`; macOS work is mostly Swift glue.

---

## E. Three-machine topology & templates

Roles (collapsible to 1 box for a demo: `gateway`+`storage`, or just `local-fs`).

| Role | Host (suggested) | Runs | Template | Status |
|---|---|---|---|---|
| `storage` | `ceph.home.arpa` | Ceph MON/OSD/RGW → bucket `dejaveph-xorbs` | (BYO — single-node `cephadm`) | ⬜ |
| `gateway` | `dejaveph.home.arpa` | `xetd` (s3 backend, tokens) + TLS proxy + opnix | `templates/gateway` | 🔵 PR #6 |
| `client` | laptop / node | `services.xetfs` (Linux) or macOS app | `templates/client` | 🔵 PR #6 |
| `demo` | one box | `xetd` local-fs + a mount, zero secrets | `templates/demo` | 🔵 PR #6 |

Deliverable: `flake.nix` `templates.{storage,gateway,client,demo}` so a user runs
`nix flake init -t github:SEBK4C/Dejaveph#gateway` and edits ~6 lines.

---

## F. Quality-of-life backlog (cheapest first)

- 🔵 Flake templates (§E) — makes deploy copy-paste. (PR #6)
- ✅ `dejaveph-doctor` + `bootstrap-ceph` helper scripts — `scripts/` on `main` (iter5).
- ⬜ Fold the helpers into a single `dejaveph` Rust CLI (`doctor`/`bootstrap-ceph`/`mount`).
- ✅ Sane defaults wired (port 9777, path-style RGW, `dejaveph-xorbs`, `home.arpa`).
- ⬜ Local-fs first-run with zero secrets.
- ⬜ Meaningful health/ready endpoint.
- ⬜ Secret auto-provisioning (opnix creates the item).

---

## Canonical naming conventions

| Thing | Value |
|---|---|
| 1Password vault | `Infrastructure` |
| RGW creds item | `dejaveph-ceph-rgw` (`access_key_id`, `secret_access_key`) |
| mount token item | `dejaveph-xetd-tokens` (`read_token`, `write_token`) |
| opnix service account | `op://Infrastructure/opnix-service-account/credential` |
| gateway host | `dejaveph.home.arpa` |
| storage/RGW endpoint | `https://rgw.ceph.home.arpa` |
| bucket | `dejaveph-xorbs` |
| RGW user | `dejaveph` (least-privilege, one bucket) |
| xetd port | `9777` |

---

## Recommended execution order

1. **Merge the open stack** (#1→#3 security, #2→#4 deployment). _human review_
2. **Flake templates** (§E) + **`dejaveph doctor`** — biggest UX win per unit effort.
3. **`register_file` file-hash verification** (B) — closes the content-poisoning MEDIUM; pairs
   naturally with the binary `mdb_shard` work.
4. **`dejaveph bootstrap-ceph`** — removes the manual Ceph step from the deploy.
5. **macOS design doc** (D) → review → **uniffi core** → **File Provider** client.
6. **HMAC-presign capability URL** (B) — the deferred auth-model refactor, its own iteration.
7. **qemu/Nix-VM harness** (C) once a nix-capable builder is available.

Ongoing: the 30-min hardening loop keeps picking one new audit angle + one fix per iteration and
logging it in DEVLOG.md; reflect each result here.

---

## Completed log

- **2026-06-15 · Iteration 1** — Audit of `main@0b289e2`. Fixed 2 HIGH (range bounds, token RNG)
  + class defenses (parking_lot, const-time tokens). Tested footer-poison hypothesis → negative.
  Shipped NixOS `services.xetd` + opnix/Ceph deployment + docs. PRs #1, #2.
- **2026-06-15 · Iteration 2** — Resource/concurrency angle: bounded `get_range`, TOCTOU-safe GC.
  Added the `xetfs` mount CLI + `services.xetfs` module (client half) with e2e smoke. PRs #3, #4.
- **2026-06-15 · Iteration 3** — Content-integrity angle: `register_file` recomputes the file
  hash from the terms' chunks and rejects mismatches (content-poisoning MEDIUM). Shipped the
  three-machine flake templates (gateway/client/demo). Full e2e (incl. m3 write-back) green.
  PRs #5, #6.
- **2026-06-15 · Iteration 4** — Capability-access angle: `/xorb-data` is now a BLAKE3-keyed,
  TTL-bounded capability URL (capability OR bearer), closing the last MEDIUM and the §5.4/§10
  doc-vs-code gap. `cap` unit tests + `m4_capability` + full e2e green. PR #7. **All HIGH+MEDIUM
  audit findings now patched** (on branches).
- **2026-06-15 · Iteration 5** — Blob write-path concurrency: unique temp name + `hard_link`
  publish fixes a corrupt-object + double-count race on concurrent identical uploads. New
  `m0_concurrent_put` (24-way race) + full e2e green. PR #8. QoL: shipped `dejaveph-doctor` +
  `bootstrap-ceph` helper scripts to `main`.
