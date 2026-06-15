//! `xetd` — self-hosted Xet CAS server. **M0 skeleton** (`Prompt.md` §4, §15).
//!
//! What works today: the CLI the E2E harness expects, an HTTP server bound to
//! `--listen`, the bound address published to `--ready-file`, and the route table for
//! the four CAS endpoints (§4.2–§4.6) plus the `--test-hooks` admin surface. Endpoint
//! bodies return `501 Not Implemented` — wire them to `xet_core::{cas_object, mdb_shard}`,
//! an index DB, and a `BlobStore` (§5/§6) to land M0.

use std::{net::SocketAddr, path::PathBuf};

use axum::{
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use clap::{Parser, ValueEnum};
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "xetd", about = "Self-hosted Xet CAS server (M0 skeleton)")]
struct Args {
    /// Bind address. `127.0.0.1:0` picks an ephemeral port (printed to --ready-file).
    #[arg(long, default_value = "127.0.0.1:0")]
    listen: String,
    /// Server data directory (staging, scratch, etc.).
    #[arg(long)]
    data_dir: PathBuf,
    /// Index DB path (SQLite, §6).
    #[arg(long)]
    db: PathBuf,
    #[arg(long, value_enum, default_value_t = Durability::Close)]
    durability: Durability,
    #[arg(long, value_enum, default_value_t = Auth::Loopback)]
    auth: Auth,
    #[arg(long, value_enum, default_value_t = Backend::LocalFs)]
    backend: Backend,
    /// BlobStore root for the local-fs backend (§5.3).
    #[arg(long)]
    blob_root: Option<PathBuf>,
    /// S3/RGW endpoint for the s3 backend (§5.4).
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
    // TODO(M0): open index DB at `args.db`; construct the BlobStore from `args.backend`
    //           (`local-fs` -> args.blob_root | `s3` -> args.s3_*). Thread both into state.
    tracing::info!(
        backend = ?args.backend, auth = ?args.auth, durability = ?args.durability,
        "xetd starting (M0 skeleton)"
    );

    let app = router(args.test_hooks);

    let listener = tokio::net::TcpListener::bind(&args.listen).await?;
    let addr: SocketAddr = listener.local_addr()?;
    tracing::info!(%addr, "listening");

    if let Some(ready) = args.ready_file.as_ref() {
        // Harness contract: it reads "http://127.0.0.1:<port>" from this file. Write to a
        // temp sibling then rename so a reader never observes a partial line.
        let tmp = ready.with_extension("tmp");
        std::fs::write(&tmp, format!("http://{addr}"))?;
        std::fs::rename(&tmp, ready)?;
    }

    axum::serve(listener, app).await?;
    Ok(())
}

/// CAS endpoints (§4.2–§4.6), plus the `--test-hooks` admin surface when enabled.
fn router(test_hooks: bool) -> Router {
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
    app
}

// --- CAS endpoint stubs (501 until wired). Section refs point at the spec contract. ---

/// §4.2 → `{ offset_into_first_range, terms, fetch_info }`.
async fn reconstruct() -> StatusCode {
    StatusCode::NOT_IMPLEMENTED
}
/// §4.3 → 200 keyed shard | 404.
async fn global_dedup() -> StatusCode {
    StatusCode::NOT_IMPLEMENTED
}
/// §4.4 → recompute Merkle root == {xorb_hash}; store; `{ was_inserted }`.
async fn put_xorb() -> StatusCode {
    StatusCode::NOT_IMPLEMENTED
}
/// §4.5 → referenced xorbs must exist; index file/CAS/global-eligible; `{ result }`.
async fn put_shard() -> StatusCode {
    StatusCode::NOT_IMPLEMENTED
}
/// §4.6 → 206 ranged xorb bytes (local-fs serving path).
async fn xorb_data() -> StatusCode {
    StatusCode::NOT_IMPLEMENTED
}

// --- Test-only control surface (behind --test-hooks). ---

async fn test_metric() -> Json<u64> {
    Json(0)
}
async fn test_noop() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "ok": true }))
}
