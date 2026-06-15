//! M0 — xorb upload + ranged serving (`Prompt.md` §4.4, §4.6).
//!
//! Builds *real* xorbs with the vendored fork (`RawXorbData` + `SerializedXorbObject`)
//! and drives the live server, asserting the four behaviors that are implemented today:
//! integrity gate, idempotency, metrics, and ranged byte serving.

mod common;
use common::*;

use bytes::Bytes;
use xet_core::cas_object::{Chunk, RawXorbData, SerializedXorbObject};

/// Serialize a real xorb of `n` deterministic 16 KiB chunks. Returns `(bytes, xorb_hash_hex)`.
fn build_xorb(seed: u64, n: usize) -> (Vec<u8>, String) {
    let chunks: Vec<Chunk> = (0..n)
        .map(|i| Chunk::new(Bytes::from(gen_blob(seed.wrapping_add(i as u64), 16 * 1024))))
        .collect();
    let raw = RawXorbData::from_chunks(&chunks, vec![0]);
    let ser = SerializedXorbObject::from_xorb(raw, /*footer=*/ true, "lz4", 0).unwrap();
    (ser.serialized_data, ser.hash.hex())
}

#[test]
fn m0_xorb_upload_and_serve() {
    let srv = Xetd::spawn();
    let c = reqwest::blocking::Client::new();
    let (bytes, hash) = build_xorb(0x1111, 4);

    // (1) first upload stores it; metric advances.
    let r1: serde_json::Value = c
        .post(srv.url(&format!("/api/v1/xorbs/default/{hash}")))
        .body(bytes.clone())
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(r1["was_inserted"], true);
    assert_eq!(srv.metric("xorb_puts"), 1);

    // (2) idempotent: re-upload returns false and does not double-count.
    let r2: serde_json::Value = c
        .post(srv.url(&format!("/api/v1/xorbs/default/{hash}")))
        .body(bytes.clone())
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(r2["was_inserted"], false);
    assert_eq!(srv.metric("xorb_puts"), 1);

    // (3) integrity gate: valid bytes, wrong URL hash => 400.
    let wrong = "0".repeat(64);
    let bad = c
        .post(srv.url(&format!("/api/v1/xorbs/default/{wrong}")))
        .body(bytes.clone())
        .send()
        .unwrap();
    assert_eq!(bad.status(), 400);

    // (4) integrity gate: garbage bytes => 400.
    let garbage = c
        .post(srv.url(&format!("/api/v1/xorbs/default/{hash}")))
        .body(vec![1u8, 2, 3, 4, 5])
        .send()
        .unwrap();
    assert_eq!(garbage.status(), 400);

    // (5) ranged GET returns the exact stored bytes (Range is inclusive; 206).
    let n = bytes.len();
    let resp = c
        .get(srv.url(&format!("/api/v1/xorb-data/{hash}")))
        .header("Range", format!("bytes=0-{}", n - 1))
        .send()
        .unwrap();
    assert_eq!(resp.status(), 206);
    assert_eq!(resp.bytes().unwrap().as_ref(), bytes.as_slice());

    // (6) a sub-range returns exactly that slice.
    let resp = c
        .get(srv.url(&format!("/api/v1/xorb-data/{hash}")))
        .header("Range", "bytes=10-19")
        .send()
        .unwrap();
    assert_eq!(resp.status(), 206);
    assert_eq!(resp.bytes().unwrap().as_ref(), &bytes[10..20]);

    // (7) unknown xorb => 404.
    let unknown = c
        .get(srv.url(&format!("/api/v1/xorb-data/{}", "1".repeat(64))))
        .header("Range", "bytes=0-0")
        .send()
        .unwrap();
    assert_eq!(unknown.status(), 404);
}
