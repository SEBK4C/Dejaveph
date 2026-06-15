//! M0 — Core CAS round-trip (`Prompt.md` §14, `m0_core_cas.rs`).
//!
//! Invariant: **ingest → reconstruct is byte-identical and `file_hash` is deterministic.**
//! Drives the real agent pipeline (Gearhash CDC, xorb serialization, file hash) against the
//! live server (integrity-gated xorb storage + reconstruction).

mod common;
use common::*;

#[test]
fn m0_round_trip() {
    let srv = Xetd::spawn();
    let ag = Agent::new(&srv);

    // A multi-xorb file (> 60 MiB pack limit) exercises xorb grouping + multi-term reconstruction.
    let data = gen_blob(0xC0FFEE, 65 * 1024 * 1024);
    let fh = ag.ingest("vol", "/a.bin", &data);
    assert_eq!(sha256(&ag.reconstruct(&fh)), sha256(&data), "reconstruction not byte-identical");

    // Content-addressed: re-ingesting identical bytes yields the same file hash.
    assert_eq!(ag.ingest("vol", "/a2.bin", &data), fh, "file hash not deterministic");

    // A small single-chunk file round-trips too (edge: one xorb, one term).
    let small = gen_blob(0x5A11, 4096);
    let fhs = ag.ingest("vol", "/small.bin", &small);
    assert_eq!(sha256(&ag.reconstruct(&fhs)), sha256(&small));

    // Contract: reconstruction of an unknown file => 404.
    let unknown = "f".repeat(64);
    let status = reqwest::blocking::get(srv.url(&format!("/api/v1/reconstructions/{unknown}")))
        .unwrap()
        .status();
    assert_eq!(status, 404);
}
