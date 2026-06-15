//! Hermetic E2E harness — **trimmed for the M0 skeleton**.
//!
//! Provides just what the conformance + M0 smokes need: deterministic data, a real
//! `xetd` child process driven over loopback HTTP, and the `xet-agent` wrapper. The
//! full harness from `Prompt.md` (the FUSE `Mount`, the S3 `testcontainers` fixture,
//! crash/fault injection helpers) arrives with M2/M4.
#![allow(dead_code)]

use std::{
    fs,
    net::TcpStream,
    path::{Path, PathBuf},
    process::{Child, Command},
    time::{Duration, Instant},
};

use tempfile::TempDir;

/// Deterministic byte source (splitmix64). Identical `(seed,len)` ⇒ identical bytes ⇒
/// identical CDC boundaries across runs — no external RNG (would version-couple them).
pub fn gen_blob(seed: u64, len: usize) -> Vec<u8> {
    let mut s = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        s = s.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = s;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        out.extend_from_slice(&z.to_le_bytes());
    }
    out.truncate(len);
    out
}

pub fn sha256(b: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    Sha256::digest(b).into()
}

/// A running `xetd` child (local-fs backend, loopback auth, test hooks on).
pub struct Xetd {
    pub base: String,
    data: TempDir,
    blob_root: PathBuf,
    child: Child,
}

impl Xetd {
    /// Spawn `xetd` on an ephemeral port and block until it publishes its ready-file.
    ///
    /// M0 covers only the local-fs/loopback configuration; the cfg-driven `spawn`
    /// (S3 backend, token auth, durability modes) returns at M3/M4.
    pub fn spawn() -> Self {
        let data = TempDir::new().unwrap();
        let blob_root = data.path().join("blobs");
        let ready = data.path().join("ready");
        let child = Command::new(env!("CARGO_BIN_EXE_xetd"))
            .arg("--listen").arg("127.0.0.1:0")
            .arg("--data-dir").arg(data.path())
            .arg("--db").arg(data.path().join("index.sqlite"))
            .arg("--backend").arg("local-fs")
            .arg("--blob-root").arg(&blob_root)
            .arg("--durability").arg("close")
            .arg("--auth").arg("loopback")
            .arg("--test-hooks")
            .arg("--ready-file").arg(&ready)
            .spawn()
            .expect("spawn xetd");
        let base = await_ready(&ready);
        Self { base, data, blob_root, child }
    }

    pub fn url(&self, p: &str) -> String {
        format!("{}{}", self.base, p)
    }
    pub fn blob_root(&self) -> &Path {
        &self.blob_root
    }

    pub fn metric(&self, name: &str) -> u64 {
        reqwest::blocking::get(self.url(&format!("/admin/test/metric/{name}")))
            .unwrap()
            .json::<u64>()
            .unwrap()
    }
    pub fn post(&self, path: &str, body: serde_json::Value) -> reqwest::blocking::Response {
        reqwest::blocking::Client::new()
            .post(self.url(path))
            .json(&body)
            .send()
            .unwrap()
    }
}

impl Drop for Xetd {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn await_ready(ready: &Path) -> String {
    let t0 = Instant::now();
    loop {
        if let Ok(s) = fs::read_to_string(ready) {
            let url = s.trim().to_string();
            if let Some(hostport) = url.strip_prefix("http://") {
                if TcpStream::connect(hostport).is_ok() {
                    return url;
                }
            }
        }
        assert!(t0.elapsed() < Duration::from_secs(10), "xetd not ready");
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Wraps the `xet-agent` library (preferred over a CLI for speed + direct file-hash access).
pub struct Agent<'a> {
    srv: &'a Xetd,
    scratch: TempDir,
}

impl<'a> Agent<'a> {
    pub fn new(srv: &'a Xetd) -> Self {
        Self { srv, scratch: TempDir::new().unwrap() }
    }
    pub fn ingest(&self, volume: &str, dest: &str, bytes: &[u8]) -> String {
        xet_agent::ingest_bytes(&self.srv.base, volume, dest, bytes, self.scratch.path())
            .unwrap()
            .file_hash_hex()
    }
    pub fn reconstruct(&self, file_hash_hex: &str) -> Vec<u8> {
        xet_agent::reconstruct(&self.srv.base, file_hash_hex).unwrap()
    }
    /// Novel bytes written to the local-fs object store during `f` (the dedup probe).
    pub fn novel_bytes_during(&self, f: impl FnOnce()) -> u64 {
        let before = dir_size_bytes(self.srv.blob_root());
        f();
        dir_size_bytes(self.srv.blob_root()).saturating_sub(before)
    }
}

fn dir_size_bytes(root: &Path) -> u64 {
    let mut n = 0;
    if let Ok(rd) = fs::read_dir(root) {
        for e in rd.flatten() {
            let m = e.metadata().unwrap();
            n += if m.is_dir() { dir_size_bytes(&e.path()) } else { m.len() };
        }
    }
    n
}

/// RAII FUSE mount of a volume. `_session` is declared first so it drops (unmounts) before
/// `dir` is removed. `AutoUnmount` is intentionally NOT used — it implicitly requests
/// `allow_other`, which needs `user_allow_other` in /etc/fuse.conf; the session's own drop
/// unmounts for our explicit-drop RAII.
pub struct Mount {
    _session: fuser::BackgroundSession,
    pub dir: TempDir,
}

pub fn mount(srv: &Xetd, volume: &str, rw: bool) -> Mount {
    let dir = TempDir::new().unwrap();
    let fs = xetfs::Xetfs::connect(&srv.base, volume, rw).unwrap();
    let mut opts = vec![fuser::MountOption::FSName("xetfs".into())];
    if !rw {
        opts.push(fuser::MountOption::RO);
    }
    let _session = fuser::spawn_mount2(fs, dir.path(), &opts).expect("mount xetfs");
    std::thread::sleep(Duration::from_millis(100)); // let the kernel finish the mount handshake
    Mount { _session, dir }
}
