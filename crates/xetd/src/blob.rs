//! `BlobStore` — immutable, content-addressed xorb persistence (`Prompt.md` §5).
//!
//! M0 ships `local-fs`; M4 adds an `s3`/Ceph-RGW backend behind the `s3` feature. Reconstruction
//! hands clients a `presign_get` URL so bulk xorb bytes are fetched **directly** from the backend
//! (off xetd's data path, §10): local-fs returns xetd's own `/xorb-data` URL; s3 returns a
//! presigned RGW/S3 URL.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use bytes::Bytes;
use xet_core::merklehash::MerkleHash;

/// Process-wide counter for unique temp-file names (see `LocalFsBlobStore::put`).
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Lightweight existence/size probe (no body fetch).
pub struct ObjectMeta {
    pub len: u64,
}

#[async_trait]
pub trait BlobStore: Send + Sync {
    /// Idempotent write of a complete xorb. Returns `false` if the key already existed
    /// (content-addressed ⇒ identical bytes), `true` if newly written.
    async fn put(&self, key: &MerkleHash, bytes: Bytes) -> Result<bool>;

    /// Existence + size without fetching the body.
    async fn head(&self, key: &MerkleHash) -> Result<Option<ObjectMeta>>;

    /// Inclusive byte range `[start, end]` (HTTP `Range` semantics).
    async fn get_range(&self, key: &MerkleHash, start: u64, end: u64) -> Result<Bytes>;

    /// A time-limited URL a client can GET (appending a `Range` header) to fetch the object
    /// directly. local-fs returns xetd's `/xorb-data` URL; s3 returns a presigned RGW/S3 URL.
    async fn presign_get(&self, key: &MerkleHash, ttl: Duration) -> Result<String>;

    /// Delete an object (GC only). Safe to call on a missing key.
    async fn delete(&self, key: &MerkleHash) -> Result<()>;
}

/// Local-filesystem backend with two-level hex fan-out: `<root>/<h0h1>/<h2h3>/<hash>`.
pub struct LocalFsBlobStore {
    root: PathBuf,
    /// xetd's externally reachable base URL, used to build `/xorb-data` presign URLs.
    public_base: String,
    /// Per-process key for signing `/xorb-data` capability URLs (§10).
    cap_key: [u8; 32],
}

impl LocalFsBlobStore {
    pub fn new(root: impl Into<PathBuf>, public_base: impl Into<String>, cap_key: [u8; 32]) -> Self {
        Self { root: root.into(), public_base: public_base.into(), cap_key }
    }

    fn path_for(&self, key: &MerkleHash) -> PathBuf {
        let h = key.hex();
        self.root.join(&h[0..2]).join(&h[2..4]).join(&h)
    }
}

#[async_trait]
impl BlobStore for LocalFsBlobStore {
    async fn put(&self, key: &MerkleHash, bytes: Bytes) -> Result<bool> {
        let path = self.path_for(key);
        // Fast path: already stored (content-addressed ⇒ identical bytes).
        if tokio::fs::try_exists(&path).await.unwrap_or(false) {
            return Ok(false);
        }
        let dir = path.parent().expect("fanned-out path always has a parent");
        tokio::fs::create_dir_all(dir).await?;

        // Unique temp name per write: two concurrent uploads of the SAME novel xorb must NOT share
        // a temp file. With a fixed `.{hash}.tmp` name the second writer's `O_TRUNC` open could
        // truncate the first writer's bytes mid-publish, persisting a corrupt object under the
        // content hash.
        let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let tmp = dir.join(format!(".{}.{}.{}.tmp", key.hex(), std::process::id(), seq));
        tokio::fs::write(&tmp, &bytes).await?;

        // Publish atomically AND idempotently via hard-link. Unlike `rename` (which silently
        // replaces), `link` returns AlreadyExists when the final already exists — so a racing
        // writer (or a prior upload) is correctly reported as `false`, and metrics/novel-byte
        // accounting count each object exactly once.
        let result = match tokio::fs::hard_link(&tmp, &path).await {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(e) => Err(e.into()),
        };
        let _ = tokio::fs::remove_file(&tmp).await; // best-effort: never leak the temp
        result
    }

    async fn head(&self, key: &MerkleHash) -> Result<Option<ObjectMeta>> {
        match tokio::fs::metadata(self.path_for(key)).await {
            Ok(m) => Ok(Some(ObjectMeta { len: m.len() })),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    async fn get_range(&self, key: &MerkleHash, start: u64, end: u64) -> Result<Bytes> {
        // Read ONLY the requested span via seek — never the whole object. Reading the full
        // (≤64 MiB) xorb per ranged request let a client amplify memory/IO with many small
        // concurrent ranges; bounding the read to [start, end] caps it to the slice size.
        use tokio::io::{AsyncReadExt, AsyncSeekExt};
        let mut f = tokio::fs::File::open(self.path_for(key)).await?;
        let file_len = f.metadata().await?.len();
        if start >= file_len {
            return Ok(Bytes::new());
        }
        let last = end.min(file_len.saturating_sub(1)); // inclusive, clamped to EOF
        let want = (last - start + 1) as usize;
        f.seek(std::io::SeekFrom::Start(start)).await?;
        let mut buf = vec![0u8; want];
        f.read_exact(&mut buf).await?;
        Ok(Bytes::from(buf))
    }

    async fn presign_get(&self, key: &MerkleHash, ttl: Duration) -> Result<String> {
        // Time-limited capability URL: signed over (hash, exp) so the bulk path needs no bearer
        // and grants access to exactly this object until it expires (§5.4, §10).
        let h = key.hex();
        let exp = crate::cap::now_unix() + ttl.as_secs();
        let sig = crate::cap::sign(&self.cap_key, &h, exp);
        Ok(format!("{}/api/v1/xorb-data/{}?exp={}&sig={}", self.public_base, h, exp, sig))
    }

    async fn delete(&self, key: &MerkleHash) -> Result<()> {
        match tokio::fs::remove_file(self.path_for(key)).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}
