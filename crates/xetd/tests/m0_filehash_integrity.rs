//! Content-addressing integrity (iter3): `register_file` recomputes the Merkle file hash from
//! the chunks its terms reference and rejects any claim that doesn't commit to that content.
//! Without this a write-token holder could bind file_hash X to arbitrary content Y (cache/
//! content poisoning); a reader of X would then get Y.

mod common;
use common::*;

use bytes::Bytes;
use serde_json::json;
use xet_core::cas_object::{Chunk, RawXorbData, SerializedXorbObject};
use xet_core::merklehash::file_hash;

/// Build a real `n`-chunk xorb and return (serialized bytes, xorb_hash_hex, correct file_hash_hex,
/// total unpacked size). The file hash is the Merkle hash over the chunks' (hash, len) — exactly
/// what a single full-xorb term commits to.
fn build(seed: u64, n: usize) -> (Vec<u8>, String, String, u64) {
    let chunks: Vec<Chunk> =
        (0..n).map(|i| Chunk::new(Bytes::from(gen_blob(seed.wrapping_add(i as u64), 16 * 1024)))).collect();
    let pairs: Vec<(_, u64)> = chunks.iter().map(|c| (c.hash, c.data.len() as u64)).collect();
    let fh = file_hash(&pairs).hex();
    let total: u64 = chunks.iter().map(|c| c.data.len() as u64).sum();
    let raw = RawXorbData::from_chunks(&chunks, vec![0]);
    let ser = SerializedXorbObject::from_xorb(raw, true, "lz4", 0).unwrap();
    (ser.serialized_data, ser.hash.hex(), fh, total)
}

fn upload(srv: &Xetd, bytes: Vec<u8>, hash: &str) {
    let c = reqwest::blocking::Client::new();
    let r: serde_json::Value =
        c.post(srv.url(&format!("/api/v1/xorbs/default/{hash}"))).body(bytes).send().unwrap().json().unwrap();
    assert_eq!(r["was_inserted"], true);
}

fn register(srv: &Xetd, file_hash_hex: &str, xorb: &str, end: u32, total: u64) -> u16 {
    srv.post(
        "/api/v1/files",
        json!({
            "file_hash": file_hash_hex,
            "total_size": total,
            "terms": [{ "xorb": xorb, "start": 0u32, "end": end, "unpacked_length": total }],
        }),
    )
    .status()
    .as_u16()
}

#[test]
fn register_binds_file_hash_to_referenced_content() {
    let srv = Xetd::spawn();

    // Two distinct contents, each a full xorb.
    let (bytes_a, xorb_a, fh_a, total_a) = build(0xA1, 4);
    let (bytes_b, xorb_b, fh_b, total_b) = build(0xB2, 3);
    upload(&srv, bytes_a, &xorb_a);
    upload(&srv, bytes_b, &xorb_b);

    // (1) correct file_hash for A's content registers.
    assert_eq!(register(&srv, &fh_a, &xorb_a, 4, total_a), 200, "correct file_hash must 200");

    // (2) POISONING ATTEMPT: bind A's file_hash to B's content -> rejected.
    assert_eq!(register(&srv, &fh_a, &xorb_b, 3, total_b), 400, "fh_a over B's terms must 400");

    // (3) a single-bit-flipped file_hash over A's own valid terms -> rejected.
    let mut bad = fh_a.clone();
    let last = bad.pop().unwrap();
    bad.push(if last == '0' { '1' } else { '0' });
    assert_eq!(register(&srv, &bad, &xorb_a, 4, total_a), 400, "tampered file_hash must 400");

    // (4) B registers correctly under its own hash (sanity: not a blanket reject).
    assert_eq!(register(&srv, &fh_b, &xorb_b, 3, total_b), 200, "correct fh_b must 200");

    // The originally-registered A still reconstructs (registration (2)/(3) didn't clobber it).
    let recon = reqwest::blocking::get(srv.url(&format!("/api/v1/reconstructions/{fh_a}"))).unwrap();
    assert_eq!(recon.status(), 200);
}
