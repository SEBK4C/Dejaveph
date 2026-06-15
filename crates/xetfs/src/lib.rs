//! `xetfs` — the reconstructing VFS (`Prompt.md` §9).
//!
//! M2: read-only mount. M3: **writable** via write-back-on-close. A file opened for write
//! gets a staging buffer (lazily seeded by reconstructing its current bytes); `write`/
//! `setattr(size)` mutate the buffer; on `fsync`/`flush`/`release` a dirty buffer is
//! re-ingested through the agent (chunk → dedup → upload → register), advancing the catalog
//! `file_hash`. Re-ingesting the full resulting bytes makes "incremental == full" hold by
//! construction, while M1 dedup keeps the upload to just the changed chunks.
//!
//! Opens use `FOPEN_DIRECT_IO` so the kernel never serves stale cached pages — reads always
//! reflect the live staging buffer (while open) or the reconstructed `file_hash`.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fuser::{
    FileAttr, FileType, Filesystem, ReplyAttr, ReplyCreate, ReplyData, ReplyDirectory, ReplyEmpty, ReplyEntry,
    ReplyOpen, ReplyWrite, Request, TimeOrNow,
};

const TTL: Duration = Duration::from_secs(1);
const ROOT: u64 = 1; // FUSE_ROOT_ID

enum Kind {
    Dir,
    File,
}

struct Node {
    name: String,
    kind: Kind,
    parent: u64,
    children: Vec<u64>,
    file_hash: Option<String>,
    size: u64,
}

/// Write-back staging for an open-for-write file.
struct Staging {
    buf: Vec<u8>,
    dirty: bool,
}

pub struct Xetfs {
    base_url: String,
    volume: String,
    nodes: HashMap<u64, Node>,
    open_files: HashMap<u64, Staging>,
    cache: Mutex<HashMap<u64, Arc<Vec<u8>>>>,
    next: u64, // next free inode (create())
    uid: u32,
    gid: u32,
}

impl Xetfs {
    /// Connect to `xetd` at `base_url`, fetch `volume`'s catalog, and build the inode tree.
    pub fn connect(base_url: &str, volume: &str, _rw: bool) -> anyhow::Result<Self> {
        let entries: serde_json::Value = reqwest::blocking::Client::new()
            .get(format!("{base_url}/api/v1/volumes/{volume}/entries"))
            .send()?
            .json()?;

        let mut nodes: HashMap<u64, Node> = HashMap::new();
        nodes.insert(
            ROOT,
            Node { name: "/".into(), kind: Kind::Dir, parent: ROOT, children: vec![], file_hash: None, size: 0 },
        );
        let mut dir_ino: HashMap<String, u64> = HashMap::new();
        let mut next: u64 = 2;

        for e in entries.as_array().cloned().unwrap_or_default() {
            let path = e["path"].as_str().unwrap_or("").trim_start_matches('/').to_string();
            if path.is_empty() {
                continue;
            }
            let file_hash = e["file_hash"].as_str().unwrap_or("").to_string();
            let size = e["size"].as_u64().unwrap_or(0);

            let comps: Vec<&str> = path.split('/').collect();
            let mut parent = ROOT;
            let mut accum = String::new();
            for (k, comp) in comps.iter().enumerate() {
                if !accum.is_empty() {
                    accum.push('/');
                }
                accum.push_str(comp);
                if k == comps.len() - 1 {
                    let ino = next;
                    next += 1;
                    nodes.insert(
                        ino,
                        Node { name: (*comp).into(), kind: Kind::File, parent, children: vec![], file_hash: Some(file_hash.clone()), size },
                    );
                    nodes.get_mut(&parent).unwrap().children.push(ino);
                } else if let Some(&ino) = dir_ino.get(&accum) {
                    parent = ino;
                } else {
                    let ino = next;
                    next += 1;
                    nodes.insert(
                        ino,
                        Node { name: (*comp).into(), kind: Kind::Dir, parent, children: vec![], file_hash: None, size: 0 },
                    );
                    nodes.get_mut(&parent).unwrap().children.push(ino);
                    dir_ino.insert(accum.clone(), ino);
                    parent = ino;
                }
            }
        }

        Ok(Self {
            base_url: base_url.to_string(),
            volume: volume.to_string(),
            nodes,
            open_files: HashMap::new(),
            cache: Mutex::new(HashMap::new()),
            next,
            uid: unsafe { libc::getuid() },
            gid: unsafe { libc::getgid() },
        })
    }

    fn attr(&self, ino: u64) -> Option<FileAttr> {
        let n = self.nodes.get(&ino)?;
        let (kind, perm, nlink) = match n.kind {
            Kind::Dir => (FileType::Directory, 0o755, 2),
            Kind::File => (FileType::RegularFile, 0o644, 1),
        };
        Some(FileAttr {
            ino,
            size: n.size,
            blocks: n.size.div_ceil(512),
            atime: UNIX_EPOCH,
            mtime: UNIX_EPOCH,
            ctime: UNIX_EPOCH,
            crtime: UNIX_EPOCH,
            kind,
            perm,
            nlink,
            uid: self.uid,
            gid: self.gid,
            rdev: 0,
            blksize: 512,
            flags: 0,
        })
    }

    /// Reconstruct a file's bytes (cached per inode).
    fn file_bytes(&self, ino: u64) -> Option<Arc<Vec<u8>>> {
        if let Some(b) = self.cache.lock().unwrap().get(&ino) {
            return Some(b.clone());
        }
        let fh = self.nodes.get(&ino)?.file_hash.clone()?;
        let bytes = xet_agent::reconstruct(&self.base_url, &fh).ok()?;
        let arc = Arc::new(bytes);
        self.cache.lock().unwrap().insert(ino, arc.clone());
        Some(arc)
    }

    /// Ensure a staging buffer exists for `ino`, seeding it from the current file bytes.
    fn ensure_staging(&mut self, ino: u64) {
        if self.open_files.contains_key(&ino) {
            return;
        }
        let buf = self.file_bytes(ino).map(|b| (*b).clone()).unwrap_or_default();
        self.open_files.insert(ino, Staging { buf, dirty: false });
    }

    fn flush_if_dirty(&mut self, ino: u64) {
        let buf = match self.open_files.get_mut(&ino) {
            Some(s) if s.dirty => {
                s.dirty = false;
                s.buf.clone()
            }
            _ => return,
        };
        self.flush_bytes(ino, &buf);
    }

    /// Re-ingest a file's staged bytes: chunk → dedup → upload novel xorbs → register.
    fn flush_bytes(&mut self, ino: u64, buf: &[u8]) {
        let path = self.full_path(ino);
        match xet_agent::ingest_bytes(&self.base_url, &self.volume, &path, buf, std::path::Path::new("")) {
            Ok(ing) => {
                let fh = ing.file_hash_hex();
                if let Some(n) = self.nodes.get_mut(&ino) {
                    n.file_hash = Some(fh);
                    n.size = buf.len() as u64;
                }
                self.cache.lock().unwrap().remove(&ino);
            }
            Err(e) => eprintln!("xetfs: flush of inode {ino} failed: {e}"),
        }
    }

    fn full_path(&self, ino: u64) -> String {
        let mut parts = Vec::new();
        let mut cur = ino;
        while cur != ROOT {
            match self.nodes.get(&cur) {
                Some(n) => {
                    parts.push(n.name.clone());
                    cur = n.parent;
                }
                None => break,
            }
        }
        parts.reverse();
        format!("/{}", parts.join("/"))
    }
}

impl Filesystem for Xetfs {
    fn lookup(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEntry) {
        let name = name.to_string_lossy().to_string();
        let children = self.nodes.get(&parent).map(|p| p.children.clone()).unwrap_or_default();
        let child = children.into_iter().find(|c| self.nodes.get(c).is_some_and(|n| n.name == name));
        match child.and_then(|c| self.attr(c)) {
            Some(attr) => reply.entry(&TTL, &attr, 0),
            None => reply.error(libc::ENOENT),
        }
    }

    fn getattr(&mut self, _req: &Request<'_>, ino: u64, reply: ReplyAttr) {
        match self.attr(ino) {
            Some(attr) => reply.attr(&TTL, &attr),
            None => reply.error(libc::ENOENT),
        }
    }

    fn open(&mut self, _req: &Request<'_>, _ino: u64, _flags: i32, reply: ReplyOpen) {
        // direct_io: bypass the kernel page cache so reads reflect live writes.
        reply.opened(0, fuser::consts::FOPEN_DIRECT_IO);
    }

    fn create(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        _flags: i32,
        reply: ReplyCreate,
    ) {
        let name = name.to_string_lossy().to_string();
        let children = self.nodes.get(&parent).map(|p| p.children.clone()).unwrap_or_default();
        let existing = children.into_iter().find(|c| self.nodes.get(c).is_some_and(|n| n.name == name));
        let ino = match existing {
            Some(i) => i,
            None => {
                let i = self.next;
                self.next += 1;
                self.nodes.insert(
                    i,
                    Node { name, kind: Kind::File, parent, children: vec![], file_hash: None, size: 0 },
                );
                self.nodes.get_mut(&parent).unwrap().children.push(i);
                i
            }
        };
        // Open for write with an empty (dirty) staging buffer so even a zero-byte create persists.
        self.open_files.insert(ino, Staging { buf: Vec::new(), dirty: true });
        match self.attr(ino) {
            Some(attr) => reply.created(&TTL, &attr, 0, 0, fuser::consts::FOPEN_DIRECT_IO),
            None => reply.error(libc::EIO),
        }
    }

    fn read(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyData,
    ) {
        let off = offset.max(0) as usize;
        // Read-your-writes: while open for write, serve from the staging buffer.
        if let Some(st) = self.open_files.get(&ino) {
            let start = off.min(st.buf.len());
            let end = start.saturating_add(size as usize).min(st.buf.len());
            reply.data(&st.buf[start..end]);
            return;
        }
        match self.file_bytes(ino) {
            Some(bytes) => {
                let start = off.min(bytes.len());
                let end = start.saturating_add(size as usize).min(bytes.len());
                reply.data(&bytes[start..end]);
            }
            None => reply.error(libc::ENOENT),
        }
    }

    fn write(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        data: &[u8],
        _write_flags: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyWrite,
    ) {
        self.ensure_staging(ino);
        let off = offset.max(0) as usize;
        let new_len = {
            let st = self.open_files.get_mut(&ino).unwrap();
            let end = off + data.len();
            if st.buf.len() < end {
                st.buf.resize(end, 0);
            }
            st.buf[off..end].copy_from_slice(data);
            st.dirty = true;
            st.buf.len() as u64
        };
        if let Some(n) = self.nodes.get_mut(&ino) {
            n.size = new_len;
        }
        reply.written(data.len() as u32);
    }

    #[allow(clippy::too_many_arguments)]
    fn setattr(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _mode: Option<u32>,
        _uid: Option<u32>,
        _gid: Option<u32>,
        size: Option<u64>,
        _atime: Option<TimeOrNow>,
        _mtime: Option<TimeOrNow>,
        _ctime: Option<SystemTime>,
        _fh: Option<u64>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<u32>,
        reply: ReplyAttr,
    ) {
        if let Some(sz) = size {
            self.ensure_staging(ino);
            {
                let st = self.open_files.get_mut(&ino).unwrap();
                st.buf.resize(sz as usize, 0);
                st.dirty = true;
            }
            if let Some(n) = self.nodes.get_mut(&ino) {
                n.size = sz;
            }
        }
        match self.attr(ino) {
            Some(a) => reply.attr(&TTL, &a),
            None => reply.error(libc::ENOENT),
        }
    }

    fn fsync(&mut self, _req: &Request<'_>, ino: u64, _fh: u64, _datasync: bool, reply: ReplyEmpty) {
        self.flush_if_dirty(ino);
        reply.ok();
    }

    fn flush(&mut self, _req: &Request<'_>, ino: u64, _fh: u64, _lock_owner: u64, reply: ReplyEmpty) {
        self.flush_if_dirty(ino);
        reply.ok();
    }

    fn release(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        _flags: i32,
        _lock_owner: Option<u64>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        self.flush_if_dirty(ino);
        self.open_files.remove(&ino);
        reply.ok();
    }

    fn readdir(&mut self, _req: &Request<'_>, ino: u64, _fh: u64, offset: i64, mut reply: ReplyDirectory) {
        let Some(node) = self.nodes.get(&ino) else {
            reply.error(libc::ENOENT);
            return;
        };
        if !matches!(node.kind, Kind::Dir) {
            reply.error(libc::ENOTDIR);
            return;
        }
        let mut entries: Vec<(u64, FileType, String)> = vec![
            (ino, FileType::Directory, ".".into()),
            (node.parent, FileType::Directory, "..".into()),
        ];
        for &c in &node.children {
            let cn = &self.nodes[&c];
            let ft = match cn.kind {
                Kind::Dir => FileType::Directory,
                Kind::File => FileType::RegularFile,
            };
            entries.push((c, ft, cn.name.clone()));
        }
        for (i, (e_ino, ft, name)) in entries.into_iter().enumerate().skip(offset as usize) {
            if reply.add(e_ino, (i + 1) as i64, ft, name) {
                break;
            }
        }
        reply.ok();
    }
}

/// Look up the current catalog `file_hash` for `path` within `volume` (§9.1).
pub fn catalog_file_hash(base_url: &str, volume: &str, path: &str) -> anyhow::Result<String> {
    let entries: serde_json::Value = reqwest::blocking::Client::new()
        .get(format!("{base_url}/api/v1/volumes/{volume}/entries"))
        .send()?
        .json()?;
    for e in entries.as_array().cloned().unwrap_or_default() {
        if e["path"].as_str() == Some(path) {
            if let Some(h) = e["file_hash"].as_str() {
                return Ok(h.to_string());
            }
        }
    }
    anyhow::bail!("no catalog entry for {volume}:{path}")
}
