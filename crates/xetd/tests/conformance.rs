//! Protocol conformance gate (`Prompt.md` §14) — **wired to the vendored fork** via `xet_core`.
//!
//! These assert the `XET-BLAKE3-GEARHASH-LZ4` suite test vectors against the real
//! `huggingface/xet-core` primitives (the SEBK4C fork). A failure here means the build is
//! not wire-compatible, so this job gates the whole pipeline.
//!
//! Fork mapping (see CLAUDE.md):
//!   - chunk hash            → `compute_data_hash` (keyed `DATA_KEY`)
//!   - straight hex          → `DataHash::as_bytes()` printed as hex
//!   - XET hash-string       → `DataHash::hex()` / `Display` (each `u64.to_le()` word ⇒ byte-swapped)
//!   - internal node         → `compute_internal_node_hash` over `"{hex} : {size}\n"` entries
//!                             (see `merklehash::aggregated_hashes::write_hash_entry`)
//!   - verification range    → `metadata_shard::chunk_verification::range_hash_from_chunks`

use xet_core::mdb_shard::chunk_verification::range_hash_from_chunks;
use xet_core::merklehash::{compute_data_hash, compute_internal_node_hash, DataHash};

#[test]
fn vec_chunk_hash() {
    let h = compute_data_hash(b"Hello World!"); // keyed BLAKE3 / DATA_KEY
    assert_eq!(
        hex::encode(h.as_bytes()), // straight hex of the raw 32 bytes
        "a29cfb08e608d4d8726dd8659a90b9134b3240d5d8e42d5fcb28e2a6e763a3e8"
    );
    assert_eq!(
        h.hex(), // byte-swapped XET hash-string form
        "d8d408e608fb9ca213b9909a65d86d725f2de4d8d540324be8a363e7a6e228cb"
    );
}

#[test]
fn vec_hash_string_roundtrip() {
    let raw: [u8; 32] = std::array::from_fn(|i| i as u8); // 0x00..0x1f
    let s = "07060504030201000f0e0d0c0b0a090817161514131211101f1e1d1c1b1a1918";
    let h = DataHash::from(raw);
    assert_eq!(h.hex(), s);
    assert_eq!(DataHash::from_hex(s).unwrap().as_bytes(), &raw);
}

#[test]
fn vec_internal_node() {
    // Children are parsed from their XET hash-string form (round-trips with `hex()`).
    let c1 = DataHash::from_hex("c28f58387a60d4aa200c311cda7c7f77f686614864f5869eadebf765d0a14a69").unwrap();
    let c2 = DataHash::from_hex("6e4e3263e073ce2c0e78cc770c361e2778db3b054b98ab65e277fc084fa70f22").unwrap();

    // Reproduce the fork's entry format: "{hash.hex()} : {size}\n" per child, then key
    // with INTERNAL_NODE_HASH. (`merged_hash_of_sequence` is pub(super), so format inline.)
    let mut buf = String::new();
    for (h, size) in [(c1, 100u64), (c2, 200u64)] {
        buf.push_str(&h.hex());
        buf.push_str(" : ");
        buf.push_str(&size.to_string());
        buf.push('\n');
    }
    let r = compute_internal_node_hash(buf.as_bytes());
    assert_eq!(r.hex(), "be64c7003ccd3cf4357364750e04c9592b3c36705dee76a71590c011766b6c14");
}

#[test]
fn vec_verification_hash() {
    // Inputs are raw (straight-hex) chunk-hash bytes.
    let a = DataHash::from_slice(&hex::decode("aad4607a38588fc2777f7cda1c310c209e86f564486186f6694aa1d065f7ebad").unwrap()).unwrap();
    let b = DataHash::from_slice(&hex::decode("2cce73e063324e6e271e360c77cc780e65ab984b053bdb78220fa74f08fc77e2").unwrap()).unwrap();
    let v = range_hash_from_chunks(&[a, b]); // raw concatenation, keyed VERIFICATION_KEY
    assert_eq!(v.hex(), "eb06a8ad81d588ac05d1d9a079232d9c1e7d0b07232fa58091caa7bf333a2768");
}

#[test]
#[ignore = "fetches xet-team/xet-spec-reference-files (network)"]
fn vec_reference_objects() {
    // For each published xorb: xet_core::cas_object parse -> recompute root == expect (the
    // server's integrity gate) and re-serialize byte-stable. Same for shards via mdb_shard.
    todo!("round-trip the reference dataset through the fork (de)serializers");
}
