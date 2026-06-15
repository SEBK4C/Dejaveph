//! Robustness fuzz of the xorb wire-format parser (iter8). `POST /xorbs` runs every uploaded
//! body through `XorbObject::validate_xorb_object` *before* the index lock, so a panic there is a
//! per-request crash on attacker-controlled bytes. This hammers the parser with deterministic
//! garbage, byte-flips, truncations, and extensions and asserts it always returns `Ok/Err` —
//! never panics, hangs, or OOMs. Deterministic (splitmix `gen_blob`) so any failure reproduces.

mod common;
use common::*;

use xet_core::cas_object::XorbObject;
use xet_core::merklehash::DataHash;

/// Validate must terminate with Ok/Err — a panic unwinds here and fails the test.
fn validate_no_panic(bytes: &[u8], hash: &DataHash) {
    let mut cur = std::io::Cursor::new(bytes);
    let _ = XorbObject::validate_xorb_object(&mut cur, hash);
}

fn pos(seed: u64, modulo: usize) -> usize {
    let g = gen_blob(seed, 8);
    (u64::from_le_bytes(g[0..8].try_into().unwrap()) as usize) % modulo.max(1)
}

#[test]
fn validate_xorb_object_is_panic_free_on_adversarial_input() {
    let (valid, hash_hex) = build_xorb(0xF0F0, 5);
    let hash = DataHash::from_hex(&hash_hex).unwrap();
    let zero = DataHash::from_hex(&"0".repeat(64)).unwrap();

    // 1) Pure random blobs of varied lengths (0..~8 KiB).
    for i in 0..3000u64 {
        let len = pos(i, 8192);
        let buf = gen_blob(i.wrapping_mul(0x9E37_79B9), len);
        validate_no_panic(&buf, &zero);
    }

    // 2) Byte-flip mutations of a valid xorb (1..8 flips each).
    for i in 0..3000u64 {
        let mut m = valid.clone();
        let nflips = 1 + (gen_blob(i, 1)[0] as usize % 8);
        for k in 0..nflips {
            let p = pos(i.wrapping_add((k as u64 + 1) * 911), m.len());
            m[p] ^= 0xFF;
        }
        validate_no_panic(&m, &hash);
    }

    // 3) Truncations (every prefix length class) + random-byte extensions.
    for i in 0..2000u64 {
        let cut = pos(i, valid.len() + 1);
        validate_no_panic(&valid[..cut], &hash);

        let mut ext = valid.clone();
        ext.extend_from_slice(&gen_blob(i.wrapping_add(7), 1 + (gen_blob(i, 2)[1] as usize % 64)));
        validate_no_panic(&ext, &hash);
    }

    // 4) Footer-targeted corruption: flip bytes only in the trailing region (the info/length
    //    fields that drive seeks) — the most likely place for an unchecked offset.
    let tail = valid.len().saturating_sub(64);
    for i in 0..2000u64 {
        let mut m = valid.clone();
        let p = tail + pos(i, valid.len() - tail);
        m[p] ^= gen_blob(i, 1)[0] | 1;
        validate_no_panic(&m, &hash);
    }
}
