//! `xetfs` — the reconstructing read-only VFS (`Prompt.md` §9).
//!
//! M2: a `fuser::Filesystem` whose tree is built from the server's VFS catalog
//! (`GET /volumes/{v}/entries`). `lookup`/`getattr`/`readdir` are served entirely from the
//! in-memory inode tree (no CAS access); `read` reconstructs the file via `xet_agent`
//! (cached per inode). A decompressed-chunk cache and writable mounts (M3) come later.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use fuser::{
    FileAttr, FileType, Filesystem, ReplyAttr, ReplyData, ReplyDirectory, ReplyEntry, ReplyOpen, Request,
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

pub struct Xetfs {
    base_url: String,
    nodes: HashMap<u64, Node>,
    cache: Mutex<HashMap<u64, Arc<Vec<u8>>>>,
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
        let mut dir_ino: HashMap<String, u64> = HashMap::new(); // accumulated path -> inode
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
                        Node {
                            name: (*comp).to_string(),
                            kind: Kind::File,
                            parent,
                            children: vec![],
                            file_hash: Some(file_hash.clone()),
                            size,
                        },
                    );
                    nodes.get_mut(&parent).unwrap().children.push(ino);
                } else if let Some(&ino) = dir_ino.get(&accum) {
                    parent = ino;
                } else {
                    let ino = next;
                    next += 1;
                    nodes.insert(
                        ino,
                        Node { name: (*comp).to_string(), kind: Kind::Dir, parent, children: vec![], file_hash: None, size: 0 },
                    );
                    nodes.get_mut(&parent).unwrap().children.push(ino);
                    dir_ino.insert(accum.clone(), ino);
                    parent = ino;
                }
            }
        }

        Ok(Self {
            base_url: base_url.to_string(),
            nodes,
            cache: Mutex::new(HashMap::new()),
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
        reply.opened(0, 0);
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
        match self.file_bytes(ino) {
            Some(bytes) => {
                let start = (offset.max(0) as usize).min(bytes.len());
                let end = start.saturating_add(size as usize).min(bytes.len());
                reply.data(&bytes[start..end]);
            }
            None => reply.error(libc::ENOENT),
        }
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
            // `add` returns true when the reply buffer is full.
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
