//! S3 / Ceph-RGW `BlobStore` (`Prompt.md` §5.4). Behind the `s3` feature.
//!
//! Xorbs are ≤ 64 MiB immutable objects, so a single `PutObject` suffices (no multipart).
//! Bulk reads go **client → presigned GET → object store** directly (off xetd's data path);
//! the presign covers the object so one URL serves any byte span via the client's `Range`
//! header. Path-style addressing is the safe default for RGW.

use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use bytes::Bytes;

use aws_sdk_s3::config::{BehaviorVersion, Credentials, Region};
use aws_sdk_s3::presigning::PresigningConfig;
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::Client;

use xet_core::merklehash::MerkleHash;

use crate::blob::{BlobStore, ObjectMeta};

pub struct S3BlobStore {
    client: Client,
    bucket: String,
}

impl S3BlobStore {
    pub async fn new(
        endpoint: &str,
        bucket: &str,
        access_key: &str,
        secret_key: &str,
        path_style: bool,
    ) -> Result<Self> {
        let creds = Credentials::new(access_key, secret_key, None, None, "xetd-static");
        let conf = aws_sdk_s3::Config::builder()
            .behavior_version(BehaviorVersion::latest())
            .region(Region::new("us-east-1")) // RGW accepts any region string
            .endpoint_url(endpoint)
            .credentials_provider(creds)
            .force_path_style(path_style)
            .build();
        Ok(Self { client: Client::from_conf(conf), bucket: bucket.to_string() })
    }

    /// Content-addressed, fanned-out object key (matches the local-fs layout, §5.2).
    fn key_for(key: &MerkleHash) -> String {
        let h = key.hex();
        format!("xorbs/{}/{}/{}", &h[0..2], &h[2..4], h)
    }
}

#[async_trait]
impl BlobStore for S3BlobStore {
    async fn put(&self, key: &MerkleHash, bytes: Bytes) -> Result<bool> {
        let k = Self::key_for(key);
        // Content-addressed idempotency: skip the write if the object already exists.
        if self.client.head_object().bucket(&self.bucket).key(&k).send().await.is_ok() {
            return Ok(false);
        }
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(&k)
            .body(ByteStream::from(bytes.to_vec()))
            .send()
            .await
            .context("s3 put_object")?;
        Ok(true)
    }

    async fn head(&self, key: &MerkleHash) -> Result<Option<ObjectMeta>> {
        match self.client.head_object().bucket(&self.bucket).key(Self::key_for(key)).send().await {
            Ok(o) => Ok(Some(ObjectMeta { len: o.content_length().unwrap_or(0).max(0) as u64 })),
            Err(err) => {
                if err.as_service_error().map(|e| e.is_not_found()).unwrap_or(false) {
                    Ok(None)
                } else {
                    Err(anyhow!("s3 head_object: {err}"))
                }
            }
        }
    }

    async fn get_range(&self, key: &MerkleHash, start: u64, end: u64) -> Result<Bytes> {
        let out = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(Self::key_for(key))
            .range(format!("bytes={start}-{end}"))
            .send()
            .await
            .context("s3 get_object")?;
        let data = out.body.collect().await.context("s3 collect body")?.into_bytes();
        Ok(data)
    }

    async fn presign_get(&self, key: &MerkleHash, ttl: Duration) -> Result<String> {
        let pc = PresigningConfig::expires_in(ttl).context("presign config")?;
        let req = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(Self::key_for(key))
            .presigned(pc)
            .await
            .context("presign get_object")?;
        Ok(req.uri().to_string())
    }

    async fn delete(&self, key: &MerkleHash) -> Result<()> {
        self.client
            .delete_object()
            .bucket(&self.bucket)
            .key(Self::key_for(key))
            .send()
            .await
            .context("s3 delete_object")?;
        Ok(())
    }
}
