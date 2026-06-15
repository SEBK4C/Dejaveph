//! `xetfs` — the reconstructing VFS (`Prompt.md` §9).
//!
//! **M0 placeholder.** The FUSE mount, inode model, and decompressed-chunk cache land
//! at M2; this crate currently only declares the catalog/connect surface the later
//! E2E harness drives. TODO(M2): add the `fuser` dependency, implement
//! `fuser::Filesystem`, and serve `lookup`/`getattr`/`readdir` from the VFS catalog
//! (§9.1) with reads driven by reconstruction (§9.3).

/// Handle to a volume to be mounted (M2+).
pub struct Xetfs {
    _base: String,
    _volume: String,
    _rw: bool,
}

impl Xetfs {
    /// Connect to `xetd` at `base_url` for `volume`. `rw` requests a writable mount (M3).
    pub fn connect(base_url: &str, volume: &str, rw: bool) -> anyhow::Result<Self> {
        Ok(Self {
            _base: base_url.to_owned(),
            _volume: volume.to_owned(),
            _rw: rw,
        })
    }
}

/// The current catalog `file_hash` for `path` within `volume` (§9.1) — the name↔hash
/// bridge the write-back tests assert against.
pub fn catalog_file_hash(_base_url: &str, _volume: &str, _path: &str) -> anyhow::Result<String> {
    // TODO(M2): query the VFS catalog (volume, path) -> file_hash hex.
    anyhow::bail!("xetfs::catalog_file_hash not yet implemented (M0 stub)")
}
