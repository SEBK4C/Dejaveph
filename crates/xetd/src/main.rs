//! `xetd` — self-hosted Xet CAS server (`Prompt.md` §4).
//!
//! M0 status: the xorb data path and a working reconstruction round-trip are real.
//! `POST /xorbs` runs the integrity gate and stores to a `BlobStore`; `POST /files`
//! registers a file's reconstruction terms; `GET /reconstructions` returns terms +
//! ranged `fetch_info`; `GET /xorb-data` serves the bytes. Global dedup (`/chunks`)
//! and binary shard upload (`/shards`) are still `501`.
//!
//! `POST /files` is an M0-internal JSON registration (file_hash → terms). The binary
//! `mdb_shard` wire format that stock `hf-xet` uses is a later refinement.

mod blob;
mod state;

use std::{collections::HashMap, net::SocketAddr, path::PathBuf, sync::Arc};

use axum::{
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, Path as AxPath, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use clap::{Parser, ValueEnum};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::atomic::Ordering::Relaxed;
use tracing_subscriber::EnvFilter;
use xet_core::cas_object::XorbObject;
use xet_core::merklehash::DataHash;

use crate::blob::{BlobStore, LocalFsBlobStore};
use crate::state::{AppState, FileRecord, Term};

/// Max accepted request body. Xorbs are ≤ `MAX_XORB_SIZE` (64 MiB); allow headroom.
const MAX_BODY_BYTES: usize = 128 * 1024 * 1024;

#[derive(Parser, Debug)]
#[command(name = "xetd", about = "Self-hosted Xet CAS server")]
struct Args {
    #[arg(long, default_value = "127.0.0.1:0")]
    listen: String,
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
    #[arg(long)]
    blob_root: Option<PathBuf>,
    #[arg(long)]
    s3_endpoint: Option<String>,
    #[arg(long)]
    s3_bucket: Option<String>,
    #[arg(long, default_value_t = false)]
    s3_path_style: bool,
    #[arg(long, default_value_t = false)]
    test_hooks: bool,
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
    tracing::info!(
        backend = ?args.backend, auth = ?args.auth, durability = ?args.durability,
        "xetd starting"
    );

    // Bind first so the bound address can seed the reconstruction `fetch_info` base URL.
    let listener = tokio::net::TcpListener::bind(&args.listen).await?;
    let addr: SocketAddr = listener.local_addr()?;
    let base_url = format!("http://{addr}");
    tracing::info!(%addr, "listening");

    let state = AppState::new(blob, base_url);
    let app = router(args.test_hooks, state);

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
        .route("/api/v1/files", post(register_file)) // M0-internal file registration
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
    app.layer(DefaultBodyLimit::max(MAX_BODY_BYTES)).with_state(state)
}

// ---------------------------------------------------------------------------
// Reconstruction wire types (§4.2). `range` is chunk-index, end-EXCLUSIVE;
// `url_range` is an HTTP byte range, end-INCLUSIVE. Do not conflate them.
// ---------------------------------------------------------------------------

#[derive(Serialize, Clone, Copy)]
struct ChunkRange {
    start: u32,
    end: u32,
}
#[derive(Serialize, Clone, Copy)]
struct ByteRange {
    start: u64,
    end: u64,
}
#[derive(Serialize)]
struct ReconTerm {
    hash: String,
    unpacked_length: u64,
    range: ChunkRange,
}
#[derive(Serialize)]
struct FetchInfo {
    range: ChunkRange,
    url: String,
    url_range: ByteRange,
}
#[derive(Serialize)]
struct ReconResponse {
    offset_into_first_range: u64,
    terms: Vec<ReconTerm>,
    fetch_info: HashMap<String, Vec<FetchInfo>>,
}

#[derive(Deserialize)]
struct RegisterTerm {
    xorb: String,
    start: u32,
    end: u32,
    unpacked_length: u64,
}
#[derive(Deserialize)]
struct RegisterFile {
    file_hash: String,
    total_size: u64,
    terms: Vec<RegisterTerm>,
    volume: Option<String>,
    path: Option<String>,
}

/// §4.2 — return a file's reconstruction: ordered terms + per-xorb ranged `fetch_info`.
async fn reconstruct(State(st): State<Arc<AppState>>, AxPath(file_hash_hex): AxPath<String>) -> Response {
    let Ok(fh) = DataHash::from_hex(&file_hash_hex) else {
        return (StatusCode::BAD_REQUEST, "malformed file hash").into_response();
    };
    let idx = st.index.lock().unwrap();
    let Some(file) = idx.files.get(&fh) else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let mut terms = Vec::with_capacity(file.terms.len());
    let mut fetch_info: HashMap<String, Vec<FetchInfo>> = HashMap::new();
    for t in &file.terms {
        let Some(meta) = idx.xorbs.get(&t.xorb) else {
            return (StatusCode::INTERNAL_SERVER_ERROR, "term references unindexed xorb").into_response();
        };
        // chunk range [start, end) -> compressed byte range using stored end-offsets (§6.2).
        let byte_start = if t.start == 0 { 0 } else { meta.boundary_offsets[(t.start - 1) as usize] as u64 };
        let byte_end = meta.boundary_offsets[(t.end - 1) as usize] as u64 - 1; // inclusive
        let xorb_hex = t.xorb.hex();
        let range = ChunkRange { start: t.start, end: t.end };
        terms.push(ReconTerm { hash: xorb_hex.clone(), unpacked_length: t.unpacked_length, range });
        fetch_info.entry(xorb_hex.clone()).or_default().push(FetchInfo {
            range,
            url: format!("{}/api/v1/xorb-data/{}", st.base_url, xorb_hex),
            url_range: ByteRange { start: byte_start, end: byte_end },
        });
    }
    Json(ReconResponse { offset_into_first_range: 0, terms, fetch_info }).into_response()
}

/// Register a file's reconstruction terms (M0-internal). Every referenced xorb must already be
/// uploaded (§4.5 ordering invariant) — else `400`.
async fn register_file(State(st): State<Arc<AppState>>, Json(req): Json<RegisterFile>) -> Response {
    let Ok(fh) = DataHash::from_hex(&req.file_hash) else {
        return (StatusCode::BAD_REQUEST, "malformed file hash").into_response();
    };
    let mut idx = st.index.lock().unwrap();

    let mut terms = Vec::with_capacity(req.terms.len());
    for t in &req.terms {
        let Ok(xorb) = DataHash::from_hex(&t.xorb) else {
            return (StatusCode::BAD_REQUEST, "malformed xorb hash").into_response();
        };
        if !idx.xorbs.contains_key(&xorb) {
            return (StatusCode::BAD_REQUEST, "term references an xorb that was not uploaded").into_response();
        }
        terms.push(Term { xorb, start: t.start, end: t.end, unpacked_length: t.unpacked_length });
    }

    let existed = idx.files.contains_key(&fh);
    idx.files.insert(fh, FileRecord { total_size: req.total_size, terms });
    if let (Some(v), Some(p)) = (req.volume, req.path) {
        idx.catalog.insert((v, p), fh);
    }
    Json(json!({ "result": if existed { 0 } else { 1 } })).into_response()
}

/// §4.4 — upload a serialized xorb. Integrity gate: recomputed Merkle root must equal the
/// `{xorb_hash}` in the URL, else `400`. Idempotent (`was_inserted`).
async fn put_xorb(
    State(st): State<Arc<AppState>>,
    AxPath((_ns, hash_hex)): AxPath<(String, String)>,
    body: Bytes,
) -> Response {
    let Ok(hash) = DataHash::from_hex(&hash_hex) else {
        return (StatusCode::BAD_REQUEST, "malformed xorb hash").into_response();
    };

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

// --- Not yet implemented. ---

/// §4.3 → keyed shard | 404.
async fn global_dedup() -> StatusCode {
    StatusCode::NOT_IMPLEMENTED
}
/// §4.5 → binary mdb_shard upload (stock-client interop). M0 uses `POST /files` instead.
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
