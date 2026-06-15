//! M1 (server slice) — the global dedup index + `GET /chunks` (`Prompt.md` §4.3, §6.3).
//!
//! Uploading a xorb indexes its chunks; a query for a known chunk reports where it lives, an
//! unknown chunk is a miss. This is the server foundation for client-side three-tier dedup
//! (the edit-locality round-trip lands when the agent learns to query this index).

mod common;
use common::*;

use bytes::Bytes;
use xet_core::cas_object::{Chunk, RawXorbData, SerializedXorbObject};

#[test]
fn m1_global_chunk_index() {
    let srv = Xetd::spawn();
    let c = reqwest::blocking::Client::new();

    // Build a xorb of 4 known chunks; remember chunk #1's hash.
    let chunks: Vec<Chunk> = (0..4)
        .map(|i| Chunk::new(Bytes::from(gen_blob(0x9000 + i, 16 * 1024))))
        .collect();
    let known = chunks[1].hash.hex();
    let raw = RawXorbData::from_chunks(&chunks, vec![0]);
    let ser = SerializedXorbObject::from_xorb(raw, true, "lz4", 0).unwrap();
    let xorb_hex = ser.hash.hex();
    let put = c
        .post(srv.url(&format!("/api/v1/xorbs/default/{xorb_hex}")))
        .body(ser.serialized_data)
        .send()
        .unwrap();
    assert!(put.status().is_success());

    // Known chunk => 200 hit pointing at the xorb and the right index.
    let resp = c
        .get(srv.url(&format!("/api/v1/chunks/default-merkledb/{known}")))
        .send()
        .unwrap();
    assert_eq!(resp.status(), 200);
    let v: serde_json::Value = resp.json().unwrap();
    assert_eq!(v["xorb"], xorb_hex);
    assert_eq!(v["chunk_index"], 1);
    assert_eq!(v["unpacked_length"], 16 * 1024);
    assert_eq!(srv.metric("chunk_query_hit"), 1);

    // Unknown chunk => 404 miss.
    let unknown = "0".repeat(64);
    let resp = c
        .get(srv.url(&format!("/api/v1/chunks/default-merkledb/{unknown}")))
        .send()
        .unwrap();
    assert_eq!(resp.status(), 404);
    assert_eq!(srv.metric("chunk_query_miss"), 1);
}
