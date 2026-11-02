// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The artifact download against a loopback mock server. No real network.

#![allow(clippy::unwrap_used)]

mod support;

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use semver::Version;
use sqler_update::{
    ArtifactKind, Download, UpdateClient, UpdateError, UpdateOffer, artifact_file_name,
    current_target, verify_file,
};
use support::{MockServer, Reply, Route, TEST_ONLY_SEED_A, TempDir, sha256_hex, trusted};

fn target() -> &'static str {
    current_target().unwrap()
}

fn name(kind: ArtifactKind) -> String {
    artifact_file_name(target(), kind)
}

fn client(server: &MockServer) -> UpdateClient {
    UpdateClient::new(
        &format!("{}/feed.json", server.url()),
        Version::new(1, 0, 0),
    )
    .unwrap()
    .with_trusted_keys(vec![trusted(&TEST_ONLY_SEED_A)])
}

fn entry(server: &MockServer, kind: ArtifactKind, body: &[u8]) -> Download {
    Download {
        url: format!("{}/dl/{}", server.url(), name(kind)),
        sha256: sha256_hex(body),
        size: body.len() as u64,
    }
}

fn offer(download: Option<Download>, bundle: Option<Download>) -> UpdateOffer {
    UpdateOffer {
        version: Version::new(1, 1, 0),
        notes_url: "https://example.com/n".into(),
        published: "2026-10-23".into(),
        download,
        bundle,
    }
}

async fn serving(kind: ArtifactKind, reply: Reply) -> MockServer {
    let path: &'static str = Box::leak(format!("/dl/{}", name(kind)).into_boxed_str());
    MockServer::start(vec![Route::new(path, reply)]).await
}

fn no_progress(_: u64, _: u64) {}

#[tokio::test]
async fn verified_archive_and_bundle_downloads() {
    for kind in [ArtifactKind::Archive, ArtifactKind::MacBundle] {
        let body = vec![7_u8; 100_000];
        let server = serving(kind, Reply::bytes(body.clone())).await;
        let e = entry(&server, kind, &body);
        let o = match kind {
            ArtifactKind::Archive => offer(Some(e.clone()), None),
            ArtifactKind::MacBundle => offer(None, Some(e.clone())),
        };
        let dir = TempDir::new(&format!("ok-{kind:?}"));
        let last = AtomicU64::new(0);
        let a = client(&server)
            .download(&o, kind, dir.path(), &|got, total| {
                assert_eq!(total, 100_000);
                last.store(got, Ordering::SeqCst);
            })
            .await
            .unwrap();
        assert_eq!(last.load(Ordering::SeqCst), 100_000);
        assert_eq!(
            (a.kind, a.size, a.sha256.as_str()),
            (kind, e.size, e.sha256.as_str())
        );
        assert_eq!(a.version, Version::new(1, 1, 0));
        assert_eq!(a.path, dir.path().join(name(kind)));
        verify_file(&a.path, &e.sha256, e.size).unwrap();
        assert!(!dir.path().join(format!("{}.part", name(kind))).exists());
    }
}

#[tokio::test]
async fn a_verified_file_is_reused_without_a_request_and_other_entries_go() {
    let kind = ArtifactKind::Archive;
    let body = b"already here".to_vec();
    let server = serving(kind, Reply::bytes(body.clone())).await;
    let o = offer(Some(entry(&server, kind, &body)), None);
    let dir = TempDir::new("reuse");
    std::fs::write(dir.path().join(name(kind)), &body).unwrap();
    std::fs::write(dir.path().join("old.part"), b"x").unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    let a = client(&server)
        .download(&o, kind, dir.path(), &no_progress)
        .await
        .unwrap();
    assert!(a.path.is_file());
    assert!(server.requests().is_empty());
    assert!(!dir.path().join("old.part").exists() && !dir.path().join("sub").exists());
}

async fn refused(reply: Reply, claimed: &[u8]) -> (UpdateError, TempDir) {
    let kind = ArtifactKind::Archive;
    let server = serving(kind, reply).await;
    let o = offer(Some(entry(&server, kind, claimed)), None);
    let dir = TempDir::new(&format!("bad-{}", sha256_hex(claimed)));
    let err = client(&server)
        .download(&o, kind, dir.path(), &no_progress)
        .await
        .unwrap_err();
    assert_eq!(server.requests().len(), 1);
    (err, dir)
}

fn assert_clean(dir: &TempDir) {
    let left: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
    assert!(left.is_empty(), "left files: {left:?}");
}

#[tokio::test]
async fn bad_bodies_are_refused_and_leave_nothing() {
    let good = b"0123456789".to_vec();
    // Content-Length differs from the signed size.
    let (e, d) = refused(Reply::bytes(b"0123".to_vec()), &good).await;
    assert_eq!(e, UpdateError::SizeMismatch);
    assert_clean(&d);
    // Longer than signed, without a length header.
    let (e, d) = refused(Reply::unframed(b"0123456789ABC"), &good).await;
    assert_eq!(e, UpdateError::SizeMismatch);
    assert_clean(&d);
    // Shorter than signed, without a length header.
    let (e, d) = refused(Reply::unframed(b"01234"), &good).await;
    assert_eq!(e, UpdateError::SizeMismatch);
    assert_clean(&d);
    // Right size, wrong bytes.
    let (e, d) = refused(Reply::bytes(b"9876543210".to_vec()), &good).await;
    assert_eq!(e, UpdateError::HashMismatch);
    assert_clean(&d);
    // Not a success status.
    let (e, d) = refused(Reply::status(404), &good).await;
    assert_eq!(e, UpdateError::Status(404));
    assert_clean(&d);
}

#[tokio::test]
async fn refusals_before_any_request() {
    let kind = ArtifactKind::Archive;
    let server = MockServer::start(Vec::new()).await;
    let body = b"abc";
    let good = entry(&server, kind, body);
    let dir = TempDir::new("pre");
    let c = client(&server);
    let go = |o: UpdateOffer, k: ArtifactKind| {
        let c = c.clone();
        let p = dir.path().to_path_buf();
        async move { c.download(&o, k, &p, &no_progress).await.unwrap_err() }
    };

    assert_eq!(go(offer(None, None), kind).await, UpdateError::NoArtifact);
    // `bundles` missing while a bundle is wanted.
    assert_eq!(
        go(offer(Some(good.clone()), None), ArtifactKind::MacBundle).await,
        UpdateError::NoArtifact
    );
    // The name of the other kind.
    let wrong_name = Download {
        url: format!("{}/dl/{}", server.url(), name(ArtifactKind::MacBundle)),
        ..good.clone()
    };
    assert_eq!(
        go(offer(Some(wrong_name), None), kind).await,
        UpdateError::UntrustedUrl
    );
    for url in [
        format!("https://evil.example/dl/{}", name(kind)),
        format!(
            "https://github.com/other/sqler/releases/download/v1.1.0/{}",
            name(kind)
        ),
        format!(
            "https://github.com/alwatroos/sqler/releases/download/v9.9.9/{}",
            name(kind)
        ),
        format!("{}/dl/{}?x=1", server.url(), name(kind)),
        format!("{}/dl/{}#f", server.url(), name(kind)),
        format!(
            "http://u:p@{}/dl/{}",
            server.url().trim_start_matches("http://"),
            name(kind)
        ),
        "not a url".to_owned(),
    ] {
        let d = Download {
            url: url.clone(),
            ..good.clone()
        };
        assert_eq!(
            go(offer(Some(d), None), kind).await,
            UpdateError::UntrustedUrl,
            "{url}"
        );
    }
    let big = Download {
        size: sqler_update::MAX_ARTIFACT_BYTES + 1,
        ..good.clone()
    };
    assert_eq!(
        go(offer(Some(big), None), kind).await,
        UpdateError::TooLarge
    );
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn the_github_release_address_is_trusted() {
    // Trusted on shape. The 1 ms stall timeout ends the request at once, so no
    // real network traffic is waited for.
    let kind = ArtifactKind::Archive;
    let server = MockServer::start(Vec::new()).await;
    let url = format!(
        "https://github.com/alwatroos/sqler/releases/download/v1.1.0/{}",
        name(kind)
    );
    let d = Download {
        url,
        sha256: sha256_hex(b"x"),
        size: 1,
    };
    let dir = TempDir::new("gh");
    let err = client(&server)
        .with_stall_timeout(Duration::from_millis(1))
        .download(&offer(Some(d), None), kind, dir.path(), &no_progress)
        .await
        .unwrap_err();
    assert_ne!(err, UpdateError::UntrustedUrl);
}

#[tokio::test]
async fn a_stalled_server_times_out_and_leaves_nothing() {
    let kind = ArtifactKind::Archive;
    let body = b"0123456789".to_vec();
    let server = serving(kind, Reply::stall()).await;
    let o = offer(Some(entry(&server, kind, &body)), None);
    let dir = TempDir::new("stall");
    let err = client(&server)
        .with_stall_timeout(Duration::from_millis(200))
        .download(&o, kind, dir.path(), &no_progress)
        .await
        .unwrap_err();
    assert_eq!(err, UpdateError::Timeout);
    assert_clean(&dir);
}

#[tokio::test]
async fn the_download_sends_the_headers_of_the_check() {
    let kind = ArtifactKind::Archive;
    let body = b"payload".to_vec();
    let path: &'static str = Box::leak(format!("/dl/{}", name(kind)).into_boxed_str());
    let server = MockServer::start(vec![
        Route::new("/feed.json", Reply::status(500)),
        Route::new(path, Reply::bytes(body.clone())),
    ])
    .await;
    let c = client(&server);
    let _ = c.check().await;
    let dir = TempDir::new("headers");
    c.download(
        &offer(Some(entry(&server, kind, &body)), None),
        kind,
        dir.path(),
        &no_progress,
    )
    .await
    .unwrap();
    let reqs = server.requests();
    assert_eq!(reqs.len(), 2);
    let names = |r: &support::Recorded| {
        let mut v: Vec<(String, String)> = r
            .headers
            .iter()
            .filter(|(k, _)| k != "host")
            .cloned()
            .collect();
        v.sort();
        v
    };
    assert_eq!(names(&reqs[0]), names(&reqs[1]));
    assert_eq!(reqs[1].method, "GET");
    assert_eq!(
        reqs[1].header("user-agent"),
        Some("SQLer/1.0.0 (update-check)")
    );
}
