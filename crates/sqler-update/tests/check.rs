// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The update check against a loopback mock server. No real network, no
//! sleeps, no elapsed-time assertions.

#![allow(clippy::unwrap_used)]

mod support;

use std::time::Duration;

use semver::Version;
use sqler_update::{CheckOutcome, UpdateClient, UpdateError, current_target};
use support::{
    MockServer, Reply, Route, TEST_ONLY_SEED_A, TEST_ONLY_SEED_B, dead_port_url, envelope_bytes,
    signed_envelope, trusted,
};

const SHA: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const FEED: &str = "/sqler-update.json";

fn manifest(version: &str) -> String {
    let target = current_target().unwrap_or("x86_64-unknown-linux-gnu");
    format!(
        r#"{{"schema":1,"product":"sqler","version":"{version}","published":"2026-10-23","notes_url":"https://example.com/notes","platforms":{{"{target}":{{"url":"https://example.com/sqler","sha256":"{SHA}","size":42}}}}}}"#
    )
}

fn client(server: &MockServer) -> UpdateClient {
    UpdateClient::new(&format!("{}{FEED}", server.url()), Version::new(1, 0, 0))
        .unwrap()
        .with_trusted_keys(vec![trusted(&TEST_ONLY_SEED_A)])
}

async fn serving(body: Vec<u8>) -> MockServer {
    MockServer::start(vec![Route::new(FEED, Reply::ok(body))]).await
}

async fn check_manifest(json: &str) -> Result<CheckOutcome, UpdateError> {
    let server = serving(signed_envelope(json, &TEST_ONLY_SEED_A)).await;
    client(&server).check().await
}

#[tokio::test]
async fn newer_version_is_available_with_its_download() {
    let outcome = check_manifest(&manifest("1.1.0")).await.unwrap();
    let CheckOutcome::Available(offer) = outcome else {
        panic!("expected Available, got {outcome:?}");
    };
    assert_eq!(offer.version, Version::new(1, 1, 0));
    assert_eq!(offer.notes_url, "https://example.com/notes");
    assert_eq!(offer.published, "2026-10-23");
    if current_target().is_some() {
        let download = offer.download.unwrap();
        assert_eq!(download.sha256, SHA);
        assert_eq!(download.size, 42);
    }
}

#[tokio::test]
async fn newer_version_without_a_platform_entry_has_no_download() {
    let json = manifest("1.1.0").replace(
        current_target().unwrap_or("x86_64-unknown-linux-gnu"),
        "riscv64-unknown-none",
    );
    let CheckOutcome::Available(offer) = check_manifest(&json).await.unwrap() else {
        panic!("expected Available");
    };
    assert!(offer.download.is_none());
}

#[tokio::test]
async fn same_older_and_prerelease_are_up_to_date() {
    for (served, expected) in [
        ("1.0.0", Version::new(1, 0, 0)),
        ("0.9.0", Version::new(0, 9, 0)),
        ("1.2.0-rc.1", Version::parse("1.2.0-rc.1").unwrap()),
    ] {
        let outcome = check_manifest(&manifest(served)).await.unwrap();
        assert_eq!(outcome, CheckOutcome::UpToDate { latest: expected });
    }
}

#[tokio::test]
async fn build_metadata_does_not_make_an_update() {
    let outcome = check_manifest(&manifest("1.0.0+2")).await.unwrap();
    assert_eq!(
        outcome,
        CheckOutcome::UpToDate {
            latest: Version::parse("1.0.0+2").unwrap()
        }
    );
    let CheckOutcome::Available(offer) = check_manifest(&manifest("1.0.1+3")).await.unwrap() else {
        panic!("expected Available");
    };
    assert_eq!(offer.version, Version::parse("1.0.1+3").unwrap());
    let outcome = check_manifest(&manifest("1.0.1")).await.unwrap();
    assert!(matches!(outcome, CheckOutcome::Available(_)));
}

#[tokio::test]
async fn flipped_payload_byte_is_a_bad_signature() {
    let mut env: serde_json::Value =
        serde_json::from_slice(&signed_envelope(&manifest("1.1.0"), &TEST_ONLY_SEED_A)).unwrap();
    let payload = manifest("1.9.0");
    env["payload"] = base64_of(&payload).into();
    let server = serving(serde_json::to_vec(&env).unwrap()).await;
    assert_eq!(
        client(&server).check().await,
        Err(UpdateError::BadSignature)
    );
}

fn base64_of(text: &str) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(text)
}

#[tokio::test]
async fn wrong_key_is_unknown_and_a_forged_key_id_is_a_bad_signature() {
    let json = manifest("1.1.0");
    // Signed by B, trusted A: the key id is B's, so it is unknown.
    let server = serving(signed_envelope(&json, &TEST_ONLY_SEED_B)).await;
    assert_eq!(client(&server).check().await, Err(UpdateError::UnknownKey));
    // Signed by B but naming A's key id: the signature does not verify.
    let forged = envelope_bytes(&json, &TEST_ONLY_SEED_B, trusted(&TEST_ONLY_SEED_A).id());
    let server = serving(forged).await;
    assert_eq!(
        client(&server).check().await,
        Err(UpdateError::BadSignature)
    );
}

#[tokio::test]
async fn rotation_accepts_either_trusted_key() {
    let server = serving(signed_envelope(&manifest("1.1.0"), &TEST_ONLY_SEED_B)).await;
    let c = client(&server)
        .with_trusted_keys(vec![trusted(&TEST_ONLY_SEED_A), trusted(&TEST_ONLY_SEED_B)]);
    assert!(matches!(c.check().await, Ok(CheckOutcome::Available(_))));
}

#[tokio::test]
async fn truncated_bodies_are_errors() {
    // The server promises more bytes than it sends.
    let lying =
        b"HTTP/1.1 200 OK\r\nContent-Length: 500\r\nConnection: close\r\n\r\n{\"format\"".to_vec();
    let server = MockServer::start(vec![Route::new(FEED, Reply::raw(lying))]).await;
    assert_eq!(client(&server).check().await, Err(UpdateError::Network));
    // Complete but cut JSON.
    let server = serving(b"{\"format\":1,\"key_id\"".to_vec()).await;
    assert!(matches!(
        client(&server).check().await,
        Err(UpdateError::Malformed(_))
    ));
}

#[tokio::test]
async fn silent_server_times_out() {
    let server = MockServer::start(vec![Route::new(FEED, Reply::stall())]).await;
    let c = client(&server)
        .with_timeouts(Duration::from_secs(5), Duration::from_millis(50))
        .unwrap();
    assert_eq!(c.check().await, Err(UpdateError::Timeout));
}

#[tokio::test]
async fn dead_port_is_offline() {
    let url = format!("{}{FEED}", dead_port_url().await);
    let c = UpdateClient::new(&url, Version::new(1, 0, 0))
        .unwrap()
        .with_trusted_keys(vec![trusted(&TEST_ONLY_SEED_A)]);
    assert_eq!(c.check().await, Err(UpdateError::Offline));
}

#[tokio::test]
async fn error_status_is_reported() {
    let server = MockServer::start(vec![Route::new(FEED, Reply::status(500))]).await;
    assert_eq!(client(&server).check().await, Err(UpdateError::Status(500)));
    let server = MockServer::start(Vec::new()).await;
    assert_eq!(client(&server).check().await, Err(UpdateError::Status(404)));
}

#[tokio::test]
async fn redirect_is_followed() {
    let server = MockServer::start(vec![
        Route::new(FEED, Reply::redirect(302, "/asset")),
        Route::new(
            "/asset",
            Reply::ok(signed_envelope(&manifest("1.1.0"), &TEST_ONLY_SEED_A)),
        ),
    ])
    .await;
    assert!(matches!(
        client(&server).check().await,
        Ok(CheckOutcome::Available(_))
    ));
    assert_eq!(server.requests().len(), 2);
}

#[tokio::test]
async fn too_many_redirects_fail() {
    let server = MockServer::start(vec![
        Route::new(FEED, Reply::redirect(302, "/r1")),
        Route::new("/r1", Reply::redirect(302, "/r2")),
        Route::new("/r2", Reply::redirect(302, "/r3")),
        Route::new("/r3", Reply::redirect(302, "/asset")),
        Route::new(
            "/asset",
            Reply::ok(signed_envelope(&manifest("1.1.0"), &TEST_ONLY_SEED_A)),
        ),
    ])
    .await;
    assert_eq!(client(&server).check().await, Err(UpdateError::Redirect));
    assert_eq!(server.requests().len(), 4);
}

#[tokio::test]
async fn redirect_to_non_loopback_http_is_refused_before_connecting() {
    let server = MockServer::start(vec![Route::new(
        FEED,
        Reply::redirect(302, "http://203.0.113.1/"),
    )])
    .await;
    assert_eq!(client(&server).check().await, Err(UpdateError::Redirect));
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn oversized_responses_are_refused() {
    // Content-Length over the cap: refused before reading.
    let server = serving(vec![b' '; 64 * 1024 + 1]).await;
    assert_eq!(client(&server).check().await, Err(UpdateError::TooLarge));
    // No Content-Length (chunked): refused by the capped read.
    let size = 64 * 1024 + 1;
    let mut raw = format!(
        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{size:x}\r\n"
    )
    .into_bytes();
    raw.extend(vec![b' '; size]);
    raw.extend_from_slice(b"\r\n0\r\n\r\n");
    let server = MockServer::start(vec![Route::new(FEED, Reply::raw(raw))]).await;
    assert_eq!(client(&server).check().await, Err(UpdateError::TooLarge));
}

#[tokio::test]
async fn not_configured_makes_no_request() {
    let server = serving(signed_envelope(&manifest("1.1.0"), &TEST_ONLY_SEED_A)).await;
    let c = client(&server).with_trusted_keys(Vec::new());
    assert!(!c.is_configured());
    assert_eq!(c.check().await, Err(UpdateError::NotConfigured));
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn default_client_is_configured_and_rejects_test_signatures() {
    // The build embeds the production key; a TEST ONLY signature has another
    // key id and is refused.
    let server = serving(signed_envelope(&manifest("1.1.0"), &TEST_ONLY_SEED_A)).await;
    let default =
        UpdateClient::new(&format!("{}{FEED}", server.url()), Version::new(1, 0, 0)).unwrap();
    assert!(default.is_configured());
    assert_eq!(default.check().await, Err(UpdateError::UnknownKey));
}

#[tokio::test]
async fn request_shape_is_minimal() {
    let server = serving(signed_envelope(&manifest("1.1.0"), &TEST_ONLY_SEED_A)).await;
    client(&server).check().await.unwrap();
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    let r = &requests[0];
    assert_eq!(r.method, "GET");
    assert_eq!(r.path, FEED);
    assert_eq!(r.query, None);
    for (name, _) in &r.headers {
        assert!(
            ["host", "user-agent", "accept"].contains(&name.as_str()),
            "unexpected header {name}"
        );
    }
    assert_eq!(r.header("user-agent"), Some("SQLer/1.0.0 (update-check)"));
}
