//! M2 — read-only FUSE mount, range reads, readdir (`Prompt.md` §14, `m2_ro_vfs.rs`).
//!
//! Invariant: **files read through the mount equal the originals**, partial reads are correct,
//! and the catalog tree is browsable. Needs `/dev/fuse` + a setuid `fusermount` (or
//! `CAP_SYS_ADMIN`); the CI job grants those.

mod common;
use common::*;

use std::io::{Read, Seek, SeekFrom};

#[test]
fn m2_ro_mount() {
    let srv = Xetd::spawn();
    let ag = Agent::new(&srv);
    let data = gen_blob(0x2222, 8 * 1024 * 1024); // multi-term within one xorb
    ag.ingest("vol", "/dir/f.bin", &data);

    let m = mount(&srv, "vol", /*rw=*/ false);
    let p = m.dir.path().join("dir/f.bin");

    // Whole-file read through the mount equals the original.
    assert_eq!(sha256(&std::fs::read(&p).unwrap()), sha256(&data));

    // Random preads return the corresponding slices (range reconstruction).
    let mut f = std::fs::File::open(&p).unwrap();
    for (off, len) in [(0usize, 1), (1_000_003, 500_001), (data.len() - 5, 5)] {
        let mut buf = vec![0u8; len];
        f.seek(SeekFrom::Start(off as u64)).unwrap();
        f.read_exact(&mut buf).unwrap();
        assert_eq!(buf, &data[off..off + len], "pread @ {off}..{}", off + len);
    }

    // readdir surfaces the catalog entry.
    let names: Vec<_> = std::fs::read_dir(m.dir.path().join("dir"))
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert!(names.iter().any(|n| n == "f.bin"), "f.bin not listed in readdir");
}
