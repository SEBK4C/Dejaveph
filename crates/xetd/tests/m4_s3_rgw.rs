//! M4 — S3 / Ceph-RGW backend, presigned direct fetch (`Prompt.md` §14, `m4_s3_rgw.rs`).
//!
//! Invariant: a **full round-trip with bulk bytes fetched directly from the object store via a
//! presigned URL** (off xetd's data path), with read-after-write and range correctness.
//!
//! `#[ignore]` — needs BOTH an S3 endpoint (MinIO or a Ceph RGW demo) AND the binary built with
//! `--features s3`. Run:
//! ```text
//!   XET_S3_ENDPOINT=http://127.0.0.1:9000 XET_S3_BUCKET=xet \
//!   AWS_ACCESS_KEY_ID=test AWS_SECRET_ACCESS_KEY=testtest12 \
//!   cargo test --features s3 --test m4_s3_rgw -- --ignored
//! ```
//! The bucket must already exist (the CI fixture / `mc mb` creates it).

mod common;
use common::*;

#[test]
#[ignore = "needs an S3 endpoint (MinIO/RGW) + a binary built with --features s3"]
fn m4_s3_round_trip_presigned() {
    let endpoint = std::env::var("XET_S3_ENDPOINT").expect("set XET_S3_ENDPOINT");
    let bucket = std::env::var("XET_S3_BUCKET").unwrap_or_else(|_| "xet".into());
    let key = std::env::var("AWS_ACCESS_KEY_ID").expect("set AWS_ACCESS_KEY_ID");
    let secret = std::env::var("AWS_SECRET_ACCESS_KEY").expect("set AWS_SECRET_ACCESS_KEY");

    let srv = Xetd::spawn_s3(&endpoint, &bucket, &key, &secret);
    let ag = Agent::new(&srv);

    let data = gen_blob(0x4D4, 12 * 1024 * 1024);
    let fh = ag.ingest("vol", "/s3.bin", &data);

    // Read-after-write: reconstructable immediately after PUT (RGW gives read-after-write for new keys).
    assert_eq!(sha256(&ag.reconstruct(&fh)), sha256(&data));

    // The presigned URLs must point at the object store directly, not back at xetd.
    let recon: serde_json::Value =
        reqwest::blocking::get(srv.url(&format!("/api/v1/reconstructions/{fh}"))).unwrap().json().unwrap();
    let fetch = recon["fetch_info"].as_object().expect("fetch_info object");
    let (_xorb, entries) = fetch.iter().next().expect("at least one xorb");
    let url = entries[0]["url"].as_str().expect("url");
    let host = endpoint.trim_start_matches("http://").trim_start_matches("https://");
    assert!(url.contains(host), "fetch must hit the object store directly (got {url})");

    // And a ranged GET against the presigned URL returns exactly that span.
    let ur = &entries[0]["url_range"];
    let (s, e) = (ur["start"].as_u64().unwrap(), ur["end"].as_u64().unwrap());
    let resp = reqwest::blocking::Client::new()
        .get(url)
        .header("Range", format!("bytes={s}-{e}"))
        .send()
        .unwrap();
    assert_eq!(resp.status(), 206, "object store should honor the ranged GET");
    assert_eq!(resp.bytes().unwrap().len() as u64, e - s + 1);
}
