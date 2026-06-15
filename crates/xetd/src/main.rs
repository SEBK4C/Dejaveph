//! `xetd` — self-hosted Xet CAS server (`Prompt.md` §4).
//!
//! M0 status: the xorb data path is real — `POST /xorbs` runs the integrity gate
//! (recompute Merkle root == claimed hash) and stores to a `BlobStore`, and
//! `GET /xorb-data` serves ranged bytes. Reconstruction, global dedup, and shard
//! registration are still `501` and land in the next M0 steps.

mod blob;
mod state;

use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use axum::{
    body::{Body, Bytes},
    extract::{Path as AxPath, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use clap::{Parser, ValueEnum};
use serde_json::json;
use std::sync::atomic::Ordering::Relaxed;
use tracing_subscriber::EnvFilter;
use xet_core::cas_object::XorbObject;
use xet_core::merklehash::DataHash;

use crate::blob::{BlobStore, LocalFsBlobStore};
use crate::state::AppState;

#[derive(Parser, Debug)]
#[command(name = "xetd", about = "Self-hosted Xet CAS server")]
struct Args {
    /// Bind address. `127.0.0.1:0` picks an ephemeral port (printed to --ready-file).
    #[arg(long, default_value = "127.0.0.1:0")]
    listen: String,
    /// Server data directory (staging, scratch, default blob root).
    #[arg(long)]
    data_dir: PathBuf,
    /// Index DB path (SQLite, §6). Reserved — M0 uses an in-memory index.
    #[arg(long)]
    db: PathBuf,
    #[arg(long, value_enum, default_value_t = Durability::Close)]
    durability: Durability,
    #[arg(long, value_enum, default_value_t = Auth::Loopback)]
    auth: Auth,
    #[arg(long, value_enum, default_value_t = Backend::LocalFs)]
    backend: Backend,
    /// BlobStore root for the local-fs backend (§5.3). Defaults to `<data-dir>/blobs`.
    #[arg(long)]
    blob_root: Option<PathBuf>,
    #[arg(long)]
    s3_endpoint: Option<String>,
    #[arg(long)]
    s3_bucket: Option<String>,
    #[arg(long, default_value_t = false)]
    s3_path_style: bool,
    /// Enable the `/admin/test/*` control surface. Never enable in production.
    #[arg(long, default_value_t = false)]
    test_hooks: bool,
    /// File to atomically publish `http://<bound-addr>` to once listening.
    #[arg(long)]
    ready_file: Option<PathBuf>,
}

#[derive(Clone, Debug, ValueEnum)]
enum Durability {
    Close,
    Fsync,
}
#[derive(Clone, Debug, ValueEnum)]
enum Auth {
    Loopback,
    Tokens,
}
#[derive(Clone, Debug, ValueEnum)]
enum Backend {
    LocalFs,
    S3,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("xetd=info")))
        .init();

    let args = Args::parse();

    let blob: Arc<dyn BlobStore> = match args.backend {
        Backend::LocalFs => {
            let root = args.blob_root.clone().unwrap_or_else(|| args.data_dir.join("blobs"));
            Arc::new(LocalFsBlobStore::new(root))
        }
        Backend::S3 => anyhow::bail!("s3 backend not implemented yet (M4)"),
    };
    let state = AppState::new(blob);
    tracing::info!(
        backend = ?args.backend, auth = ?args.auth, durability = ?args.durability,
        "xetd starting"
    );

    let app = router(args.test_hooks, state);

    let listener = tokio::net::TcpListener::bind(&args.listen).await?;
    let addr: SocketAddr = listener.local_addr()?;
    tracing::info!(%addr, "listening");

    if let Some(ready) = args.ready_file.as_ref() {
        let tmp = ready.with_extension("tmp");
        std::fs::write(&tmp, format!("http://{addr}"))?;
        std::fs::rename(&tmp, ready)?;
    }

    axum::serve(listener, app).await?;
    Ok(())
}

fn router(test_hooks: bool, state: Arc<AppState>) -> Router {
    let mut app = Router::new()
        .route("/api/v1/reconstructions/{file_hash}", get(reconstruct)) // §4.2
        .route("/api/v1/chunks/{namespace}/{chunk_hash}", get(global_dedup)) // §4.3
        .route("/api/v1/xorbs/{namespace}/{xorb_hash}", post(put_xorb)) // §4.4
        .route("/api/v1/shards", post(put_shard)) // §4.5
        .route("/api/v1/xorb-data/{xorb_hash}", get(xorb_data)); // §4.6

    if test_hooks {
        app = app
            .route("/admin/test/metric/{name}", get(test_metric))
            .route("/admin/test/fault", post(test_noop))
            .route("/admin/test/crash_at", post(test_noop))
            .route("/admin/test/dedup_key", post(test_noop))
            .route("/admin/test/gc", post(test_noop))
            .route("/admin/test/scrub", post(test_noop));
    }
    app.with_state(state)
}

/// §4.4 — upload a serialized xorb. Integrity gate: the recomputed Merkle root must equal
/// the `{xorb_hash}` in the URL, else `400`. Idempotent: a re-upload returns
/// `was_inserted: false`.
async fn put_xorb(
    State(st): State<Arc<AppState>>,
    AxPath((_ns, hash_hex)): AxPath<(String, String)>,
    body: Bytes,
) -> Response {
    let Ok(hash) = DataHash::from_hex(&hash_hex) else {
        return (StatusCode::BAD_REQUEST, "malformed xorb hash").into_response();
    };

    // Integrity gate: validate structure + recompute the hash, reject on mismatch.
    let info = {
        let mut cursor = std::io::Cursor::new(body.as_ref());
        match XorbObject::validate_xorb_object(&mut cursor, &hash) {
            Ok(Some(xorb)) => xorb.info,
            Ok(None) => {
                return (StatusCode::BAD_REQUEST, "xorb failed integrity check (hash mismatch or malformed)")
                    .into_response()
            }
            Err(_) => return (StatusCode::BAD_REQUEST, "could not parse xorb").into_response(),
        }
    };

    let len = body.len() as u64;
    let inserted = match st.blob.put(&hash, body).await {
        Ok(v) => v,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("blob store: {e}")).into_response(),
    };
    if inserted {
        st.index.lock().unwrap().put_xorb(hash, &info);
        st.metrics.xorb_puts.fetch_add(1, Relaxed);
        st.metrics.novel_bytes.fetch_add(len, Relaxed);
    }
    Json(json!({ "was_inserted": inserted })).into_response()
}

/// §4.6 — serve ranged xorb bytes (local-fs path). `Range` is inclusive; emits `206`.
async fn xorb_data(
    State(st): State<Arc<AppState>>,
    AxPath(hash_hex): AxPath<String>,
    headers: HeaderMap,
) -> Response {
    let Ok(hash) = DataHash::from_hex(&hash_hex) else {
        return (StatusCode::BAD_REQUEST, "malformed xorb hash").into_response();
    };
    let Some(meta) = st.blob.head(&hash).await.ok().flatten() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some((start, end)) = parse_range(headers.get("range"), meta.len) else {
        return StatusCode::RANGE_NOT_SATISFIABLE.into_response();
    };
    let bytes = match st.blob.get_range(&hash, start, end).await {
        Ok(b) => b,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("read: {e}")).into_response(),
    };
    Response::builder()
        .status(StatusCode::PARTIAL_CONTENT)
        .header("Content-Range", format!("bytes {start}-{end}/{}", meta.len))
        .header("Accept-Ranges", "bytes")
        .header("ETag", format!("\"{hash_hex}\""))
        .header("Cache-Control", "public, immutable, max-age=300")
        .body(Body::from(bytes))
        .unwrap()
}

/// Parse an inclusive `bytes=START-END` range against `total`. Absent header ⇒ whole object.
/// Returns `None` for an unsatisfiable/malformed range.
fn parse_range(h: Option<&HeaderValue>, total: u64) -> Option<(u64, u64)> {
    let Some(h) = h else {
        return Some((0, total.saturating_sub(1)));
    };
    let spec = h.to_str().ok()?.strip_prefix("bytes=")?;
    let (a, b) = spec.split_once('-')?;
    let start: u64 = a.parse().ok()?;
    let end: u64 = if b.is_empty() { total.saturating_sub(1) } else { b.parse().ok()? };
    if start > end || start >= total {
        return None;
    }
    Some((start, end.min(total.saturating_sub(1))))
}

// --- Not yet implemented (next M0 steps). ---

/// §4.2 → terms + fetch_info.
async fn reconstruct() -> StatusCode {
    StatusCode::NOT_IMPLEMENTED
}
/// §4.3 → keyed shard | 404.
async fn global_dedup() -> StatusCode {
    StatusCode::NOT_IMPLEMENTED
}
/// §4.5 → referenced xorbs must exist; `{ result }`.
async fn put_shard() -> StatusCode {
    StatusCode::NOT_IMPLEMENTED
}

// --- Test-only control surface (behind --test-hooks). ---

async fn test_metric(State(st): State<Arc<AppState>>, AxPath(name): AxPath<String>) -> Json<u64> {
    Json(st.metrics.get(&name))
}
async fn test_noop() -> Json<serde_json::Value> {
    Json(json!({ "ok": true }))
}
