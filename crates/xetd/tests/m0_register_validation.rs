//! Regression — `register_file` must bound term chunk ranges so a crafted file can't drive
//! `reconstruct` into an out-of-bounds index *inside the held index Mutex*, poisoning it and
//! bricking every later request (HIGH-1). Each malformed range must be rejected at
//! registration (`400`), and the server must stay healthy throughout.

mod common;
use common::*;

use serde_json::json;

/// Register a file with a single term over `xorb`, returning the HTTP status.
fn register(srv: &Xetd, file_hash: &str, xorb: &str, start: u32, end: u32) -> u16 {
    srv.post(
        "/api/v1/files",
        json!({
            "file_hash": file_hash,
            "total_size": 1u64,
            "terms": [{ "xorb": xorb, "start": start, "end": end, "unpacked_length": 1u64 }],
        }),
    )
    .status()
    .as_u16()
}

#[test]
fn register_rejects_out_of_range_terms_and_server_survives() {
    let srv = Xetd::spawn();

    // A real 4-chunk xorb ⇒ valid chunk-index range is 0 <= start < end <= 4.
    let (bytes, xorb) = build_xorb(0x5151, 4);
    let c = reqwest::blocking::Client::new();
    let r: serde_json::Value = c
        .post(srv.url(&format!("/api/v1/xorbs/default/{xorb}")))
        .body(bytes)
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(r["was_inserted"], true);

    let fh = "a".repeat(64);

    // (1) end > num_chunks — the original OOB-panic trigger.
    assert_eq!(register(&srv, &fh, &xorb, 0, 5), 400, "end past num_chunks must 400");
    // (2) end == 0 — the (end-1) underflow trigger.
    assert_eq!(register(&srv, &fh, &xorb, 0, 0), 400, "end==0 must 400");
    // (3) start >= end — empty/inverted range.
    assert_eq!(register(&srv, &fh, &xorb, 3, 2), 400, "start>=end must 400");
    // (4) start == end.
    assert_eq!(register(&srv, &fh, &xorb, 2, 2), 400, "start==end must 400");

    // The server must still be alive — if any bad term had poisoned the index Mutex, this
    // metric call (which locks nothing, but reconstruct below does) and the reconstruct would
    // hang/500. Prove liveness end-to-end.
    assert_eq!(srv.metric("xorb_puts"), 1, "server still serving after rejected registers");

    // (5) a valid term registers and reconstructs without panicking.
    assert_eq!(register(&srv, &fh, &xorb, 0, 4), 200, "valid range must 200");
    let recon = c.get(srv.url(&format!("/api/v1/reconstructions/{fh}"))).send().unwrap();
    assert_eq!(recon.status(), 200, "valid file reconstructs");

    // (6) a never-registered file is a clean 404, not a panic.
    let miss = c.get(srv.url(&format!("/api/v1/reconstructions/{}", "b".repeat(64)))).send().unwrap();
    assert_eq!(miss.status(), 404);

    // Final liveness check: index Mutex was never poisoned.
    assert_eq!(srv.metric("xorb_puts"), 1);
}
