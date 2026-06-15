//! Concurrency/durability of the blob write path (iter5). Many simultaneous uploads of the SAME
//! novel xorb must (a) never persist a corrupt object — a fixed temp name let a second writer
//! truncate the first's bytes mid-publish — and (b) count as exactly one insertion (hard-link
//! publish reports racers as `was_inserted:false`, so metrics aren't double-counted).

mod common;
use common::*;

use std::sync::Arc;
use std::thread;

#[test]
fn concurrent_identical_uploads_are_idempotent_and_uncorrupted() {
    let srv = Arc::new(Xetd::spawn());
    let (bytes, hash) = build_xorb(0xC0FFEE, 6);
    let bytes = Arc::new(bytes);

    // Fire N concurrent POSTs of the exact same xorb.
    const N: usize = 24;
    let mut handles = Vec::new();
    for _ in 0..N {
        let srv = srv.clone();
        let bytes = bytes.clone();
        let hash = hash.clone();
        handles.push(thread::spawn(move || {
            let r: serde_json::Value = reqwest::blocking::Client::new()
                .post(srv.url(&format!("/api/v1/xorbs/default/{hash}")))
                .body((*bytes).clone())
                .send()
                .unwrap()
                .json()
                .unwrap();
            r["was_inserted"].as_bool().unwrap()
        }));
    }
    let inserts: usize = handles.into_iter().map(|h| h.join().unwrap() as usize).sum();

    // Exactly one writer should have reported a real insert; the rest are idempotent no-ops.
    assert_eq!(inserts, 1, "exactly one upload should report was_inserted:true");
    assert_eq!(srv.metric("xorb_puts"), 1, "metric must not double-count concurrent uploads");

    // The stored object is byte-exact (not truncated/corrupted by a temp-file collision).
    let resp = reqwest::blocking::Client::new()
        .get(srv.url(&format!("/api/v1/xorb-data/{hash}")))
        .header("Range", format!("bytes=0-{}", bytes.len() - 1))
        .send()
        .unwrap();
    assert_eq!(resp.status(), 206);
    assert_eq!(resp.bytes().unwrap().as_ref(), bytes.as_slice(), "served bytes must match exactly");
}
