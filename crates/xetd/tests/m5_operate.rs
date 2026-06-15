//! M5 — operate: GC, scrub, token scopes, metrics (`Prompt.md` §14, `m5_operate.rs`).
//!
//! Invariant: orphan GC reclaims unreferenced xorbs but spares referenced ones; scrub detects
//! on-disk corruption; scopes are enforced (read token rejected on write); metrics surface.

mod common;
use common::*;

#[test]
fn m5_gc_scrub_scopes_metrics() {
    let srv = Xetd::spawn_tokens();

    // The agent authenticates via XETD_TOKEN (write scope) for ingest/reconstruct.
    let wtok = srv.mint_token("write");
    std::env::set_var("XETD_TOKEN", &wtok);
    let ag = Agent::new(&srv);
    let c = reqwest::blocking::Client::new();

    // A referenced file...
    let d = gen_blob(0x5A, 8 * 1024 * 1024);
    let fh = ag.ingest("vol", "/keep.bin", &d);

    // ...and an orphan xorb (uploaded directly, never referenced by a file).
    let (orphan, orphan_hash) = build_xorb(0x0B, 2);
    assert!(c
        .post(srv.url(&format!("/api/v1/xorbs/default/{orphan_hash}")))
        .bearer_auth(&wtok)
        .body(orphan)
        .send()
        .unwrap()
        .status()
        .is_success());

    // GC mark-sweep: orphan reclaimed, referenced file spared.
    let rep: serde_json::Value =
        srv.post("/admin/test/gc", serde_json::json!({ "mode": "mark_sweep" })).json().unwrap();
    assert!(rep["swept"].as_u64().unwrap() >= 1, "GC should sweep the orphan");
    assert_eq!(sha256(&ag.reconstruct(&fh)), sha256(&d), "referenced file must survive GC");
    assert_eq!(
        c.get(srv.url(&format!("/api/v1/xorb-data/{orphan_hash}")))
            .bearer_auth(&wtok)
            .header("Range", "bytes=0-0")
            .send()
            .unwrap()
            .status(),
        404,
        "swept orphan must be gone"
    );

    // Scrub detects deliberate on-disk corruption.
    corrupt_one_xorb_byte(srv.blob_root());
    let report: serde_json::Value = srv.post("/admin/test/scrub", serde_json::json!({})).json().unwrap();
    assert!(report["quarantined"].as_u64().unwrap() >= 1, "scrub must flag the corrupted xorb");

    // Scope enforcement: a read-scope token is rejected on write; a write-scope token is accepted.
    let rtok = srv.mint_token("read");
    let (xb, xh) = build_xorb(0x01, 2);
    let denied = c
        .post(srv.url(&format!("/api/v1/xorbs/default/{xh}")))
        .bearer_auth(&rtok)
        .body(xb.clone())
        .send()
        .unwrap();
    assert_eq!(denied.status(), 403, "read-scope token must be rejected on write");
    let allowed = c
        .post(srv.url(&format!("/api/v1/xorbs/default/{xh}")))
        .bearer_auth(&wtok)
        .body(xb)
        .send()
        .unwrap();
    assert!(allowed.status().is_success(), "write-scope token must be accepted");

    // Metrics surface.
    assert!(srv.metric("xorb_puts") > 0);

    std::env::remove_var("XETD_TOKEN");
}
