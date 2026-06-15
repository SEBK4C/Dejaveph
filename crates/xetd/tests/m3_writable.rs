//! M3 — writable VFS: write-back-on-close (`Prompt.md` §14, `m3_writable.rs`).
//!
//! The decisive probe is **incremental == full**: an in-place mid-file overwrite through the
//! mount must yield the same `file_hash` as ingesting the resulting bytes from scratch — which
//! validates the whole write-back chain (staging → re-chunk → dedup → register) in one
//! assertion. Plus truncate/append. (Crash recovery and GC refcounts are later refinements.)

mod common;
use common::*;

use std::io::{Seek, SeekFrom, Write};

#[test]
fn m3_write_back_incremental_equals_full() {
    let srv = Xetd::spawn();
    let ag = Agent::new(&srv);
    let base = gen_blob(0x3000, 24 * 1024 * 1024);
    let f0 = ag.ingest("vol", "/w.bin", &base);

    let m = mount(&srv, "vol", /*rw=*/ true);
    let p = m.dir.path().join("w.bin");

    // In-place mid-file overwrite (no size change) via the mount, then fsync.
    let patch = gen_blob(0xBEEF, 512 * 1024);
    let at = base.len() / 2;
    {
        let mut f = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
        f.seek(SeekFrom::Start(at as u64)).unwrap();
        f.write_all(&patch).unwrap();
        f.sync_all().unwrap(); // fsync -> write-back-on-close
    }
    let mut expect = base.clone();
    expect[at..at + patch.len()].copy_from_slice(&patch);

    // Read-your-writes through the mount.
    assert_eq!(sha256(&std::fs::read(&p).unwrap()), sha256(&expect), "read-your-writes failed");

    // Catalog advanced.
    let f1 = xetfs::catalog_file_hash(&srv.base, "vol", "/w.bin").unwrap();
    assert_ne!(f1, f0, "catalog file_hash did not advance after the edit");

    // Independent reconstruction matches.
    assert_eq!(sha256(&ag.reconstruct(&f1)), sha256(&expect), "reconstruction of edited file diverged");

    // DECISIVE: the in-place edit's file_hash equals a from-scratch ingest of the same bytes.
    let f_full = ag.ingest("vol2", "/w.bin", &expect);
    assert_eq!(f1, f_full, "incremental re-chunk/splice diverged from a full ingest");
}

#[test]
fn m3_truncate_and_append() {
    let srv = Xetd::spawn();
    let ag = Agent::new(&srv);
    ag.ingest("vol", "/t.bin", &gen_blob(0x8, 3 * 1024 * 1024));

    let m = mount(&srv, "vol", true);
    let p = m.dir.path().join("t.bin");

    // truncate to 1000 bytes
    {
        let f = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
        f.set_len(1000).unwrap();
        f.sync_all().unwrap();
    }
    assert_eq!(std::fs::metadata(&p).unwrap().len(), 1000, "truncate size wrong");

    // append "TAIL" at EOF (explicit seek avoids O_APPEND/direct_io ambiguity)
    {
        let mut f = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
        f.seek(SeekFrom::Start(1000)).unwrap();
        f.write_all(b"TAIL").unwrap();
        f.sync_all().unwrap();
    }
    let v = std::fs::read(&p).unwrap();
    assert_eq!(v.len(), 1004, "append length wrong");
    assert_eq!(&v[1000..], b"TAIL", "appended bytes wrong");

    // The persisted file reconstructs independently.
    let fh = xetfs::catalog_file_hash(&srv.base, "vol", "/t.bin").unwrap();
    assert_eq!(ag.reconstruct(&fh).len(), 1004);
}
