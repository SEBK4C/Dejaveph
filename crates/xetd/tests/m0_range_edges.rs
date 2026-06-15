//! Edge cases for ranged xorb serving after `get_range` was rewritten to seek+read only the
//! requested span (bounded memory; no whole-file read). Asserts byte-exact slices across the
//! tricky boundaries: last byte, single byte, open-ended range, and a range clamped past EOF.

mod common;
use common::*;

fn put(srv: &Xetd, seed: u64, n: usize) -> (Vec<u8>, String) {
    let (bytes, hash) = build_xorb(seed, n);
    let c = reqwest::blocking::Client::new();
    let r: serde_json::Value = c
        .post(srv.url(&format!("/api/v1/xorbs/default/{hash}")))
        .body(bytes.clone())
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(r["was_inserted"], true);
    (bytes, hash)
}

fn range(srv: &Xetd, hash: &str, spec: &str) -> (u16, Vec<u8>) {
    let resp = reqwest::blocking::Client::new()
        .get(srv.url(&format!("/api/v1/xorb-data/{hash}")))
        .header("Range", spec)
        .send()
        .unwrap();
    let status = resp.status().as_u16();
    (status, resp.bytes().unwrap().to_vec())
}

#[test]
fn ranged_reads_are_byte_exact_at_boundaries() {
    let srv = Xetd::spawn();
    let (bytes, hash) = put(&srv, 0x7e57, 3);
    let n = bytes.len();

    // whole object
    let (s, b) = range(&srv, &hash, &format!("bytes=0-{}", n - 1));
    assert_eq!(s, 206);
    assert_eq!(b, bytes);

    // last single byte
    let (s, b) = range(&srv, &hash, &format!("bytes={}-{}", n - 1, n - 1));
    assert_eq!(s, 206);
    assert_eq!(b, vec![bytes[n - 1]]);

    // first single byte
    let (s, b) = range(&srv, &hash, "bytes=0-0");
    assert_eq!(s, 206);
    assert_eq!(b, vec![bytes[0]]);

    // mid sub-range
    let (s, b) = range(&srv, &hash, "bytes=100-199");
    assert_eq!(s, 206);
    assert_eq!(b, bytes[100..200].to_vec());

    // open-ended suffix range -> to EOF
    let (s, b) = range(&srv, &hash, "bytes=50-");
    assert_eq!(s, 206);
    assert_eq!(b, bytes[50..].to_vec());

    // end past EOF clamps to the last byte (server clamps in parse_range + get_range)
    let (s, b) = range(&srv, &hash, &format!("bytes=0-{}", n + 10_000));
    assert_eq!(s, 206);
    assert_eq!(b, bytes);

    // start past EOF is unsatisfiable
    let (s, _) = range(&srv, &hash, &format!("bytes={}-{}", n + 1, n + 5));
    assert_eq!(s, 416);
}
