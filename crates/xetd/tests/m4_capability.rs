//! Capability-URL access control for `/xorb-data` in tokens mode (iter4, §10). Reconstruction
//! hands back a time-limited signed URL; the bulk fetch then needs NO bearer. An unsigned or
//! tampered request with no bearer is forbidden; a bearer still works (capability OR bearer).

mod common;
use common::*;

use serde_json::Value;

#[test]
fn xorb_data_requires_capability_or_bearer_in_tokens_mode() {
    let srv = Xetd::spawn_tokens();
    let wtok = srv.mint_token("write");
    let rtok = srv.mint_token("read");

    // Ingest a file (agent authenticates via XETD_TOKEN=write).
    std::env::set_var("XETD_TOKEN", &wtok);
    let ag = Agent::new(&srv);
    let data = gen_blob(0xC4, 256 * 1024);
    let fh = ag.ingest("vol", "/f.bin", &data);
    std::env::remove_var("XETD_TOKEN");

    let c = reqwest::blocking::Client::new();

    // Reconstruction (read bearer) → signed fetch_info URL.
    let recon: Value = c
        .get(srv.url(&format!("/api/v1/reconstructions/{fh}")))
        .bearer_auth(&rtok)
        .send()
        .unwrap()
        .json()
        .unwrap();
    let url = recon["fetch_info"]
        .as_object()
        .unwrap()
        .values()
        .next()
        .unwrap()[0]["url"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(url.contains("exp=") && url.contains("sig="), "reconstruction URL must be signed: {url}");

    // (1) signed URL, NO bearer → served (capability self-authorizes).
    let r = c.get(&url).header("Range", "bytes=0-0").send().unwrap();
    assert_eq!(r.status(), 206, "signed capability URL must serve without a bearer");

    // The bare path (no query) for the unsigned cases.
    let bare = url.split('?').next().unwrap().to_string();

    // (2) no signature, no bearer → forbidden.
    let r = c.get(&bare).header("Range", "bytes=0-0").send().unwrap();
    assert_eq!(r.status(), 403, "unsigned + no bearer must be 403");

    // (3) no signature but a valid bearer → served (capability OR bearer).
    let r = c.get(&bare).bearer_auth(&rtok).header("Range", "bytes=0-0").send().unwrap();
    assert_eq!(r.status(), 206, "valid bearer must still serve xorb-data");

    // (4) tampered signature, no bearer → forbidden (flip the last hex char of the sig).
    let last = url.chars().last().unwrap();
    let flipped = if last == '0' { '1' } else { '0' };
    let tampered = format!("{}{}", &url[..url.len() - 1], flipped);
    let r = c.get(&tampered).header("Range", "bytes=0-0").send().unwrap();
    assert_eq!(r.status(), 403, "tampered signature must be 403");
}
