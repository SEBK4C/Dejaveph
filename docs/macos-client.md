# Dejaveph macOS client — design doc

Status: **draft for review** (iteration 8). No code yet — this is the plan to critique before
building. Companion: [`Plan&Execution.md`](Plan&Execution.md) §D, [`DEPLOYMENT.md`](DEPLOYMENT.md).

Goal: a **Dropbox-like** macOS experience over a Dejaveph volume — a folder in Finder whose files
are reconstructed on demand from the chunk-deduplicated CAS, with sync-status badges, on-demand
download, and edits that write back. No kernel extension, App-Store-shippable.

---

## 1. The core decision: File Provider, not FUSE

| | FUSE (macFUSE) | **File Provider (chosen)** |
|---|---|---|
| Kernel extension | Yes — user must approve a kext in System Settings, reduced-security boot | **No kext** |
| Finder integration | A plain mount | Native: sync badges, "Download Now", dataless placeholders |
| On-demand materialization | Manual | **Built in** (the OS manages placeholders + eviction) |
| Distribution | Hard (kext signing) | Notarized app / App Store |
| Maps to our model | Adequate | **1:1** (see §3) |

We use Apple's **`NSFileProviderReplicatedExtension`** (macOS 11+) — the same framework behind the
modern Dropbox, iCloud Drive, and OneDrive clients. The synced volume appears under
`~/Library/CloudStorage/Dejaveph-<volume>`. The OS owns the on-disk placeholders and calls our
extension to enumerate, fetch content, and apply changes.

**Why it fits so well:** File Provider tracks each item's `contentVersion`; when it changes, the
OS re-fetches. In Dejaveph the file's **content version *is* its `file_hash`** — a content
address that changes iff the bytes change. So eviction/refresh is free and correct by
construction, and the CAS makes re-materialization cheap (dedup).

A **FUSE-T MVP** (no kext, NFS-based) is kept as a fast fallback — see §6 Tier 0.

---

## 2. Architecture

```
┌──────────────────────────────────────────────────────────┐
│  Dejaveph.app  (SwiftUI menu-bar app)                      │
│   • sign-in (server URL + token from 1Password)            │
│   • add/remove volumes → register File Provider domains    │
│   • status: per-volume sync state, recent activity, cache  │
└───────────────┬──────────────────────────────────────────┘
                │ registers domains / shares config (App Group)
┌───────────────▼──────────────────────────────────────────┐
│  FileProviderExtension  (NSFileProviderReplicatedExtension)│
│   enumerator → item() → fetchContents() → modify/create    │
└───────────────┬──────────────────────────────────────────┘
                │ Swift ↔ Rust (uniffi-generated bindings)
┌───────────────▼──────────────────────────────────────────┐
│  libxetagent  (Rust, = the existing xet-agent via uniffi)  │
│   list_entries · reconstruct(file_hash) · ingest(bytes)    │
│   reqwest + rustls (https://) · same CDC/BLAKE3/xorb code   │
└───────────────┬──────────────────────────────────────────┘
                │ HTTPS (caddy) + presigned RGW GETs
┌───────────────▼──────────────────────────────────────────┐
│  xetd gateway  ──►  Ceph RGW                                │
└──────────────────────────────────────────────────────────┘
```

**The reuse win:** `libxetagent` is the *existing* `xet-agent` crate exposed through
[`uniffi`](https://mozilla.github.io/uniffi-rs/). The exact chunking (Gearhash), hashing
(BLAKE3), xorb (de)serialization, dedup, and reconstruction that the Linux server and tests use
run **unchanged** on macOS — byte-for-byte protocol-compatible, zero reimplementation. The Swift
side is glue: translate File Provider callbacks ↔ `libxetagent` calls.

---

## 3. Data-flow mapping (File Provider ⇄ Dejaveph)

| File Provider concept | Dejaveph |
|---|---|
| Domain | one xetd **volume** (`Dejaveph-models`, `Dejaveph-scratch`, …) |
| Enumerate a directory | `GET /api/v1/volumes/{v}/entries`, build the item tree from `path` |
| Item identifier | the catalog `path` (stable); root = `NSFileProviderRootContainerItemIdentifier` |
| `item.itemVersion.contentVersion` | the file's **`file_hash`** |
| `fetchContents(for:)` | `libxetagent.reconstruct(file_hash)` → write to temp file → return URL |
| materialize a placeholder | OS calls `fetchContents` on first read; ranged for huge files |
| `createItem` / `modifyItem` (new contents) | `libxetagent.ingest(volume, path, bytes)` → new `file_hash` → `POST /files` → return item with the new contentVersion |
| `deleteItem` | remove the catalog entry (needs a `DELETE` route — see §7) |
| eviction (reclaim disk) | drop the local copy; next access re-reconstructs (cheap, immutable) |
| change-based sync (`enumerateChanges`) | diff the volume catalog vs the last anchor (needs a change cursor — §7) |

**Read path:** Finder shows dataless placeholders from enumeration. First open → OS calls
`fetchContents` → Rust reconstructs from the CAS (ranged GETs to presigned RGW URLs) → file
materializes. Identical content across edits dedups; eviction is safe because `file_hash` pins
the bytes.

**Write path:** user saves → OS calls `modifyItem` with the new file URL → Rust ingests the whole
file (chunk → dedup → upload only novel xorbs → register), exactly the M3 `incremental == full`
model already proven on Linux → returns the new `file_hash` as the contentVersion. Only changed
chunks upload.

---

## 4. `libxetagent` (the uniffi core)

Expose a small, stable surface from `xet-agent` (new crate `crates/xetagent-ffi`, or a uniffi
feature on `xet-agent`):

```
// UDL / proc-macro sketch — not final
interface XetClient {
    constructor(string server, string? bearer_token);   // https:// + 1Password token
    sequence<Entry> list_entries(string volume);
    bytes reconstruct(string file_hash);                 // whole-file; ranged variant below
    bytes reconstruct_range(string file_hash, u64 offset, u64 len);
    FileHash ingest(string volume, string path, bytes data);
};
dictionary Entry { string path; string file_hash; u64 size; };
```

- Built as an **xcframework** (arm64 + x86_64) via `uniffi-bindgen` + `cargo` (or `cargo-swift`).
- Network: reqwest + **rustls** (already enabled in the crates as of iter7) → talks to the caddy
  `https://` gateway.
- `reconstruct_range` matters: File Provider can request partial content for large files; we
  already return ranged `fetch_info`, so partial materialization is natural.
- Keep it **sync** (blocking) inside the extension's background queues, or add async later.

---

## 5. Auth & secrets (mirror the NixOS/1Password pattern)

- **Bearer token** comes from **1Password on macOS**, three viable sources (pick per
  environment): the **1Password SDK** (service-account token), the **`op` CLI** (`op read
  op://Infrastructure/dejaveph-xetd-tokens/write_token`), or a one-time paste the app stores in
  the **macOS Keychain** (shared via App Group so the extension can read it). Never written to
  disk in plaintext — same principle as opnix rendering to tmpfs on NixOS.
- **TLS:** the gateway is `https://dejaveph.home.arpa` (caddy). For `tls internal` (Caddy's local
  CA on the `home.arpa` zone) the user trusts Caddy's root CA in the macOS Keychain once; for a
  public domain, ACME certs validate automatically.
- **Canonical names:** vault `Infrastructure`, token item `dejaveph-xetd-tokens` (fields
  `read_token` / `write_token`), host `dejaveph.home.arpa`. A read-only mount uses `read_token`; a
  writable one uses `write_token`.

---

## 6. Delivery tiers (ship incrementally)

- **Tier 0 — MVP (days):** menu-bar app wrapping the existing **`xetfs` CLI** over **FUSE-T**
  (userspace, no kext). Proves the UX end-to-end; not Finder-native. Good for a demo / dogfood.
- **Tier 1 — File Provider, read-only (≈1–2 wks):** `libxetagent` xcframework + a replicated
  extension doing enumeration + `item()` + `fetchContents`. Real Finder placeholders, on-demand
  download, eviction. **This is the "looks like Dropbox" milestone.**
- **Tier 2 — File Provider, writable (≈1–2 wks):** `createItem` / `modifyItem` / `deleteItem` →
  ingest on save. The decisive feature; `incremental == full` carries over from M3.
- **Tier 3 — polish:** `enumerateChanges` for live multi-device sync, conflict policy (last-writer
  or conflict-copy), offline queue, status UI, cache/eviction tuning, signing + notarization.

---

## 7. What the server still needs (gaps this surfaces)

The macOS client is a forcing function for a few server features already on the roadmap:

1. **`DELETE` a catalog entry** — File Provider `deleteItem` needs it (today only register/list).
2. **Change cursor for `enumerateChanges`** — a per-volume monotonically-increasing change token
   so the extension can sync deltas instead of re-listing. Pairs with the versioned-catalog idea.
3. **Per-volume / per-user auth scoping** — tokens are global today (security backlog §B). True
   multi-user macOS sync wants volume-scoped tokens (or real JWT, §A refinements).
4. **Conflict semantics** — the catalog is mutable last-writer-wins rows; concurrent multi-client
   edits need a defined policy (the schema "leaves room for a versioned/commit model" per
   `CLAUDE.md`).

None block Tier 0/1 (read-only); (1)+(3) are needed for a good Tier 2; (2)+(4) for Tier 3.

---

## 8. Risks / open questions

- **Apple plumbing:** File Provider needs an Apple Developer account, the File Provider
  entitlement, app+extension signing, and notarization to distribute. Non-trivial but standard.
- **Replicated-extension quirks:** enumeration anchors, the "working set," and `signalEnumerator`
  semantics have sharp edges; budget time for the first integration.
- **Huge files:** rely on `reconstruct_range` + let the OS request partial content; avoid loading
  whole files in the extension.
- **Cross-platform reuse:** the same `libxetagent` uniffi core can back a future **Windows**
  client (Cloud Filter API / `CfApi`) — keep the Rust surface platform-neutral.
- **FUSE-T licensing:** FUSE-T is free for personal use but check terms before shipping Tier 0
  broadly; it's a stopgap, not the product.

---

## 9. First concrete step (proposed for the next iteration)

Stand up **`libxetagent`**: a `uniffi` wrapper crate over `xet-agent` exposing `list_entries` /
`reconstruct` / `ingest`, building to a macOS xcframework, with a tiny Swift CLI harness that
lists a volume and reconstructs one file over `https://`. That validates the whole Rust↔Swift↔CAS
spine before any File Provider UI — the riskiest integration, de-risked first. Then Tier 1.
