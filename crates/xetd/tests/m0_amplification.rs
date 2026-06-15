//! Amplification-DoS guard (iter6). A `register_file` body is small per term (~110 bytes) but
//! each term can reference up to a full xorb's chunk range, so a modest body could expand the
//! server's `file_pairs` allocation (and the file-hash work) to billions of entries. The server
//! caps the total referenced chunk count and rejects early — before the big allocation, and
//! before the file_hash check.

mod common;
use common::*;

use serde_json::json;

#[test]
fn register_rejects_chunk_count_amplification() {
    let srv = Xetd::spawn();

    // A 256-chunk xorb; one full-range term references 256 chunks.
    let (bytes, xorb) = build_xorb(0xA11, 256);
    let c = reqwest::blocking::Client::new();
    let r: serde_json::Value = c
        .post(srv.url(&format!("/api/v1/xorbs/default/{xorb}")))
        .body(bytes)
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(r["was_inserted"], true);

    // 8000 terms × 256 chunks = 2,048,000 > the 2,000,000 cap. The guard fires mid-loop and
    // returns BEFORE the file_hash check, so a bogus file_hash is irrelevant here.
    let terms: Vec<_> = (0..8000)
        .map(|_| json!({ "xorb": xorb, "start": 0, "end": 256, "unpacked_length": 1u64 }))
        .collect();
    let resp = srv.post(
        "/api/v1/files",
        json!({ "file_hash": "a".repeat(64), "total_size": 1u64, "terms": terms }),
    );
    let status = resp.status().as_u16();
    let body = resp.text().unwrap();
    assert_eq!(status, 400, "over-cap registration must 400");
    assert!(body.contains("too many chunks"), "must be the amplification guard, got: {body}");

    // Server is still healthy (the guard returns cleanly, no OOM/panic).
    assert_eq!(srv.metric("xorb_puts"), 1);

    // A small, in-bounds registration with the CORRECT file_hash still works (sanity that the cap
    // didn't break normal use). Use the agent for a real round-trip.
    let ag = Agent::new(&srv);
    let data = gen_blob(0x1234, 64 * 1024);
    let fh = ag.ingest("vol", "/ok.bin", &data);
    assert_eq!(sha256(&ag.reconstruct(&fh)), sha256(&data));
}
