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
#[cfg(feature = "s3")]
mod s3;
mod state;

use std::{collections::HashMap, net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

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

    tracing::info!(
        backend = ?args.backend, auth = ?args.auth, durability = ?args.durability,
        "xetd starting"
    );

    // Bind first so the bound address seeds the local-fs presign base URL.
    let listener = tokio::net::TcpListener::bind(&args.listen).await?;
    let addr: SocketAddr = listener.local_addr()?;
    let base_url = format!("http://{addr}");
    tracing::info!(%addr, "listening");

    let blob: Arc<dyn BlobStore> = match args.backend {
        Backend::LocalFs => {
            let root = args.blob_root.clone().unwrap_or_else(|| args.data_dir.join("blobs"));
            Arc::new(LocalFsBlobStore::new(root, base_url.clone()))
        }
        Backend::S3 => construct_s3(&args).await?,
    };
    let state = AppState::new(blob);
    let app = router(args.test_hooks, state);

    if let Some(ready) = args.ready_file.as_ref() {
        let tmp = ready.with_extension("tmp");
        std::fs::write(&tmp, format!("http://{addr}"))?;
        std::fs::rename(&tmp, ready)?;
    }

    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(feature = "s3")]
async fn construct_s3(args: &Args) -> anyhow::Result<Arc<dyn BlobStore>> {
    use anyhow::Context;
    let endpoint = args.s3_endpoint.clone().context("--s3-endpoint is required for the s3 backend")?;
    let bucket = args.s3_bucket.clone().context("--s3-bucket is required for the s3 backend")?;
    let access = std::env::var("AWS_ACCESS_KEY_ID").context("AWS_ACCESS_KEY_ID not set")?;
    let secret = std::env::var("AWS_SECRET_ACCESS_KEY").context("AWS_SECRET_ACCESS_KEY not set")?;
    Ok(Arc::new(
        s3::S3BlobStore::new(&endpoint, &bucket, &access, &secret, args.s3_path_style).await?,
    ))
}

#[cfg(not(feature = "s3"))]
async fn construct_s3(_args: &Args) -> anyhow::Result<Arc<dyn BlobStore>> {
    anyhow::bail!("s3 backend not enabled; rebuild with --features s3")
}

fn router(test_hooks: bool, state: Arc<AppState>) -> Router {
    let mut app = Router::new()
        .route("/api/v1/reconstructions/{file_hash}", get(reconstruct)) // §4.2
        .route("/api/v1/chunks/{namespace}/{chunk_hash}", get(global_dedup)) // §4.3
        .route("/api/v1/xorbs/{namespace}/{xorb_hash}", post(put_xorb)) // §4.4
        .route("/api/v1/shards", post(put_shard)) // §4.5
        .route("/api/v1/files", post(register_file)) // M0-internal file registration
        .route("/api/v1/volumes/{volume}/entries", get(list_entries)) // VFS catalog listing (§9.1)
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
    // Collect everything needed under the index lock; presign afterwards (it's async, and we
    // must not hold a std Mutex guard across an await). chunk range [start,end) -> compressed
    // byte range via the stored end-offsets (§6.2).
    struct Resolved {
        xorb: DataHash,
        start: u32,
        end: u32,
        unpacked: u64,
        byte_start: u64,
        byte_end: u64,
    }
    let collected: Result<Vec<Resolved>, StatusCode> = {
        let idx = st.index.lock().unwrap();
        match idx.files.get(&fh) {
            None => Err(StatusCode::NOT_FOUND),
            Some(file) => {
                let mut v = Vec::with_capacity(file.terms.len());
                let mut bad = None;
                for t in &file.terms {
                    let Some(meta) = idx.xorbs.get(&t.xorb) else {
                        bad = Some(StatusCode::INTERNAL_SERVER_ERROR);
                        break;
                    };
                    let byte_start = if t.start == 0 { 0 } else { meta.boundary_offsets[(t.start - 1) as usize] as u64 };
                    let byte_end = meta.boundary_offsets[(t.end - 1) as usize] as u64 - 1; // inclusive
                    v.push(Resolved { xorb: t.xorb, start: t.start, end: t.end, unpacked: t.unpacked_length, byte_start, byte_end });
                }
                match bad {
                    Some(code) => Err(code),
                    None => Ok(v),
                }
            }
        }
    };
    let collected = match collected {
        Ok(v) => v,
        Err(code) => return code.into_response(),
    };

    let ttl = Duration::from_secs(900);
    let mut terms = Vec::with_capacity(collected.len());
    let mut fetch_info: HashMap<String, Vec<FetchInfo>> = HashMap::new();
    for t in collected {
        let url = match st.blob.presign_get(&t.xorb, ttl).await {
            Ok(u) => u,
            Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("presign: {e}")).into_response(),
        };
        let xorb_hex = t.xorb.hex();
        let range = ChunkRange { start: t.start, end: t.end };
        terms.push(ReconTerm { hash: xorb_hex.clone(), unpacked_length: t.unpacked, range });
        fetch_info.entry(xorb_hex).or_default().push(FetchInfo {
            range,
            url,
            url_range: ByteRange { start: t.byte_start, end: t.byte_end },
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

#[derive(Serialize)]
struct EntryOut {
    path: String,
    file_hash: String,
    size: u64,
}

/// List a volume's catalog entries (path → file_hash + size) for the VFS mount (§9.1).
async fn list_entries(State(st): State<Arc<AppState>>, AxPath(volume): AxPath<String>) -> Response {
    let idx = st.index.lock().unwrap();
    let mut out = Vec::new();
    for ((vol, path), fh) in idx.catalog.iter() {
        if vol == &volume {
            let size = idx.files.get(fh).map(|f| f.total_size).unwrap_or(0);
            out.push(EntryOut { path: path.clone(), file_hash: fh.hex(), size });
        }
    }
    Json(out).into_response()
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
        {
            let mut idx = st.index.lock().unwrap();
            idx.put_xorb(hash, &info);
            idx.index_chunks(hash, &info); // populate the global dedup index
        }
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

/// §4.3 — global dedup lookup. M0-internal JSON: where the chunk lives, or `404`. (The spec
/// returns a binary keyed shard; that lands with the keyed-shard dedup protocol.)
async fn global_dedup(
    State(st): State<Arc<AppState>>,
    AxPath((_ns, chunk_hex)): AxPath<(String, String)>,
) -> Response {
    let Ok(ch) = DataHash::from_hex(&chunk_hex) else {
        return (StatusCode::BAD_REQUEST, "malformed chunk hash").into_response();
    };
    let hit = st.index.lock().unwrap().chunk_index.get(&ch).map(|loc| {
        json!({ "xorb": loc.xorb.hex(), "chunk_index": loc.index, "unpacked_length": loc.unpacked_len })
    });
    match hit {
        Some(body) => {
            st.metrics.chunk_query_hit.fetch_add(1, Relaxed);
            Json(body).into_response()
        }
        None => {
            st.metrics.chunk_query_miss.fetch_add(1, Relaxed);
            StatusCode::NOT_FOUND.into_response()
        }
    }
}

// --- Not yet implemented. ---

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
