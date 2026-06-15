//! `BlobStore` — immutable, content-addressed xorb persistence (`Prompt.md` §5).
//!
//! M0 ships `local-fs`; M4 adds an `s3`/Ceph-RGW backend behind the `s3` feature. Reconstruction
//! hands clients a `presign_get` URL so bulk xorb bytes are fetched **directly** from the backend
//! (off xetd's data path, §10): local-fs returns xetd's own `/xorb-data` URL; s3 returns a
//! presigned RGW/S3 URL.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use bytes::Bytes;
use xet_core::merklehash::MerkleHash;

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
}

/// Local-filesystem backend with two-level hex fan-out: `<root>/<h0h1>/<h2h3>/<hash>`.
pub struct LocalFsBlobStore {
    root: PathBuf,
    /// xetd's externally reachable base URL, used to build `/xorb-data` presign URLs.
    public_base: String,
}

impl LocalFsBlobStore {
    pub fn new(root: impl Into<PathBuf>, public_base: impl Into<String>) -> Self {
        Self { root: root.into(), public_base: public_base.into() }
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
        if tokio::fs::try_exists(&path).await.unwrap_or(false) {
            return Ok(false);
        }
        let dir = path.parent().expect("fanned-out path always has a parent");
        tokio::fs::create_dir_all(dir).await?;
        let tmp = dir.join(format!(".{}.tmp", key.hex()));
        tokio::fs::write(&tmp, &bytes).await?;
        match tokio::fs::rename(&tmp, &path).await {
            Ok(()) => Ok(true),
            Err(_) if tokio::fs::try_exists(&path).await.unwrap_or(false) => {
                let _ = tokio::fs::remove_file(&tmp).await;
                Ok(false)
            }
            Err(e) => Err(e.into()),
        }
    }

    async fn head(&self, key: &MerkleHash) -> Result<Option<ObjectMeta>> {
        match tokio::fs::metadata(self.path_for(key)).await {
            Ok(m) => Ok(Some(ObjectMeta { len: m.len() })),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    async fn get_range(&self, key: &MerkleHash, start: u64, end: u64) -> Result<Bytes> {
        let data = tokio::fs::read(self.path_for(key)).await?;
        let lo = (start as usize).min(data.len());
        let hi = (end as usize + 1).min(data.len());
        Ok(Bytes::copy_from_slice(&data[lo..hi.max(lo)]))
    }

    async fn presign_get(&self, key: &MerkleHash, _ttl: Duration) -> Result<String> {
        Ok(format!("{}/api/v1/xorb-data/{}", self.public_base, key.hex()))
    }
}
