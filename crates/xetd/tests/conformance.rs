//! Protocol conformance gate (`Prompt.md` §14, `tests/conformance.rs`).
//!
//! These assert the `XET-BLAKE3-GEARHASH-LZ4` suite test vectors. A failure here means
//! the build is not wire-compatible, so this job gates the whole pipeline.
//!
//! **Skeleton status:** ignored stubs. Each carries its expected vector and the
//! vendored-fork module to wire it to; remove `#[ignore]` once implemented.
//!
//! Fork module map (see CLAUDE.md):
//!   - chunk hash + hash-string  → `xet_core::merklehash`
//!   - internal node / Merkle    → `xet_core::merklehash` (`aggregated_hashes`)
//!   - verification-range hash    → `xet_core::mdb_shard` (`chunk_verification`)
//!   - xorb (de)serialize         → `xet_core::cas_object` (`xorb_object_format`)
//!   - shard (de)serialize        → `xet_core::mdb_shard` (`shard_format`)

#[test]
#[ignore = "M0: wire to xet_core::merklehash (chunk hash + hash-string)"]
fn vec_chunk_hash() {
    // keyed BLAKE3(DATA_KEY, "Hello World!")
    //   hex          = a29cfb08e608d4d8726dd8659a90b9134b3240d5d8e42d5fcb28e2a6e763a3e8
    //   hash-string  = d8d408e608fb9ca213b9909a65d86d725f2de4d8d540324be8a363e7a6e228cb
    todo!("compute via xet_core::merklehash and assert both forms");
}

#[test]
#[ignore = "M0: wire to xet_core::merklehash (hash-string round-trip)"]
fn vec_hash_string_roundtrip() {
    // raw 0x00..0x1f  <->  "07060504030201000f0e0d0c0b0a090817161514131211101f1e1d1c1b1a1918"
    todo!("assert hash-string to_string / from_string round-trip (byte-swapped form)");
}

#[test]
#[ignore = "M0: wire to xet_core::merklehash (internal node, INTERNAL_NODE_KEY)"]
fn vec_internal_node() {
    // internal_node([(c1,100),(c2,200)]) — "{str} : {size}\n" per child —
    //   string = be64c7003ccd3cf4357364750e04c9592b3c36705dee76a71590c011766b6c14
    todo!("assert internal-node aggregated hash");
}

#[test]
#[ignore = "M0: wire to xet_core::mdb_shard chunk_verification (VERIFICATION_KEY)"]
fn vec_verification_hash() {
    // verification_hash([a,b]) over the raw concatenation of chunk hashes —
    //   string = eb06a8ad81d588ac05d1d9a079232d9c1e7d0b07232fa58091caa7bf333a2768
    todo!("assert verification-range hash");
}

#[test]
#[ignore = "M0: reference objects — fetches xet-team/xet-spec-reference-files"]
fn vec_reference_objects() {
    // For each published xorb: xet_core::cas_object::parse -> recompute_root == expect
    //   (the server's integrity gate) and re-serialize byte-stable.
    // For each published shard: xet_core::mdb_shard parse -> serialize byte-stable.
    todo!("round-trip the reference dataset through the fork (de)serializers");
}
