//! M0 — Core CAS round-trip + the four wire contracts (`Prompt.md` §14, `m0_core_cas.rs`).
//!
//! Invariant: **ingest → reconstruct is byte-identical and `file_hash` is deterministic**,
//! plus the four endpoint contracts (integrity gate, idempotent PUT, ordering, 404).
//!
//! **Skeleton status:** ignored stub. `Xetd::spawn()` already boots the real server, but
//! its endpoints return 501 and `xet_agent` ingest/reconstruct are unimplemented, so the
//! assertions can't run yet. Remove `#[ignore]` as M0 lands.

mod common;
#[allow(unused_imports)]
use common::*;

#[test]
#[ignore = "M0: implement xet_agent ingest/reconstruct + the 4 endpoints, then assert"]
fn m0_smoke() {
    let _srv = Xetd::spawn(); // boots the M0 skeleton (binds, writes ready-file)

    // Target assertions once wired:
    //   let ag = Agent::new(&_srv);
    //   let data = gen_blob(0xC0FFEE, 80 << 20);              // > MAX_XORB_SIZE => multi-xorb
    //   let fh = ag.ingest("vol", "/a.bin", &data);
    //   assert_eq!(sha256(&ag.reconstruct(&fh)), sha256(&data)); // byte-identical
    //   assert_eq!(ag.ingest("vol", "/a2.bin", &data), fh);      // content-addressed
    //   Contract 1: wrong xorb hash            => 400 (integrity gate, §4.4)
    //   Contract 2: repeat xorb PUT            => was_inserted=false (idempotent)
    //   Contract 3: shard referencing absent xorb => 400 (ordering invariant, §4.5)
    //   Contract 4: unknown reconstruction     => 404 (§4.2)
    todo!("drive Agent ingest/reconstruct + the four endpoint contracts");
}
