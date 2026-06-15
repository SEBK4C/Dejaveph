//! M1 — edit locality (`Prompt.md` §14, `m1_dedup.rs`).
//!
//! Invariant: **a small mid-file edit re-uploads only the perturbed neighborhood**, and both
//! versions reconstruct correctly. This is the deduplication proof — the agent dedups against
//! the server's global chunk index (`GET /chunks`) and references existing xorbs for the
//! unchanged runs, uploading only the novel chunks around the edit.

mod common;
use common::*;

#[test]
fn m1_edit_locality() {
    let srv = Xetd::spawn();
    let ag = Agent::new(&srv);

    let a = gen_blob(0xA, 16 * 1024 * 1024);
    let fa = ag.ingest("vol", "/x", &a);

    // a' = a with a single byte inserted at the midpoint (shifts the entire second half).
    let mut ap = a.clone();
    ap.insert(a.len() / 2, 0x5A);

    // Ingesting a' must upload only a few chunks (perturbed chunk + CDC re-convergence),
    // NOT the whole file — everything else dedups against a's already-stored xorb.
    let novel = ag.novel_bytes_during(|| {
        let _ = ag.ingest("vol", "/x2", &ap);
    });
    assert!(novel < 1024 * 1024, "uploaded {novel} B for a 1-byte insert; dedup regressed");
    assert!((novel as f64) < 0.10 * ap.len() as f64, "novel {novel} B exceeds 10% of the file");

    // Both versions still reconstruct byte-for-byte.
    let fap = ag.ingest("vol", "/x3", &ap);
    assert_eq!(sha256(&ag.reconstruct(&fa)), sha256(&a), "a no longer reconstructs");
    assert_eq!(sha256(&ag.reconstruct(&fap)), sha256(&ap), "a' does not reconstruct");
    assert_ne!(fa, fap, "edited file must have a different content hash");
}
