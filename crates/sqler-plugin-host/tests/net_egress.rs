// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `NetEgress` against an in-process hyper server on 127.0.0.1:0. No external network.
#![cfg(feature = "net")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::convert::Infallible;
use std::io;
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use sqler_plugin_api::{Capability, CapabilitySet, PluginId};
use sqler_plugin_host::net::{NetEgressFactory, NetTimeouts, Resolver};
use sqler_plugin_host::{
    DenyReason, Egress, EgressError, EgressFactory, EgressRequest, EgressResponse,
    MAX_REQUEST_BODY, MAX_RESPONSE_BODY,
};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// Every await is bounded: a hang fails the test instead of the run.
async fn guard<F: std::future::Future>(f: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(30), f)
        .await
        .expect("hang guard")
}

struct Server {
    port: u16,
    hits: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve() -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let h = hits.clone();
    let task = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let h = h.clone();
            tokio::spawn(async move {
                let svc = service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
                    let h = h.clone();
                    async move {
                        h.fetch_add(1, Ordering::SeqCst);
                        Ok::<_, Infallible>(handle(req).await)
                    }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), svc)
                    .await;
            });
        }
    });
    Server { port, hits, task }
}

async fn handle(req: hyper::Request<hyper::body::Incoming>) -> hyper::Response<Full<Bytes>> {
    let path = req.uri().path().to_owned();
    let host = req
        .headers()
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let has_proxy_auth = req.headers().contains_key("proxy-authorization");
    let body = req.into_body().collect().await.unwrap().to_bytes();
    let ok = |b: Bytes| hyper::Response::new(Full::new(b));
    match path.as_str() {
        "/redirect" => hyper::Response::builder()
            .status(302)
            .header("location", "http://denied.example.com/")
            .body(Full::new(Bytes::new()))
            .unwrap(),
        "/big" => ok(Bytes::from(vec![0u8; MAX_RESPONSE_BODY + 1])),
        "/max" => ok(Bytes::from(vec![1u8; MAX_RESPONSE_BODY])),
        "/slow" => {
            tokio::time::sleep(Duration::from_secs(20)).await;
            ok(Bytes::new())
        }
        "/echo" => ok(Bytes::from(format!(
            "host={host};proxy_auth={has_proxy_auth};body={}",
            body.len()
        ))),
        _ => ok(Bytes::from_static(b"hello")),
    }
}

/// A resolver that answers from a fixed table and counts lookups.
struct Table {
    answers: Vec<(&'static str, Vec<IpAddr>)>,
    lookups: Arc<AtomicUsize>,
}

impl Resolver for Table {
    fn lookup(&self, host: &str) -> sqler_plugin_host::net::LookupFuture {
        self.lookups.fetch_add(1, Ordering::SeqCst);
        let found = self
            .answers
            .iter()
            .find(|(n, _)| *n == host)
            .map(|(_, a)| a.clone());
        Box::pin(
            async move { found.ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "nxdomain")) },
        )
    }
}

fn caps(items: &[String]) -> CapabilitySet {
    items
        .iter()
        .map(|s| s.parse::<Capability>().unwrap())
        .collect()
}

fn egress_with(
    pattern: &str,
    resolver: Arc<dyn Resolver>,
    timeouts: NetTimeouts,
) -> Arc<dyn Egress> {
    let id: PluginId = "demo".parse().unwrap();
    NetEgressFactory::with_parts(resolver, timeouts)
        .for_plugin(&id, &caps(&[format!("net.http:{pattern}")]))
}

fn table(answers: Vec<(&'static str, Vec<IpAddr>)>) -> (Arc<dyn Resolver>, Arc<AtomicUsize>) {
    let lookups = Arc::new(AtomicUsize::new(0));
    (
        Arc::new(Table {
            answers,
            lookups: lookups.clone(),
        }),
        lookups,
    )
}

fn get(uri: &str) -> EgressRequest {
    EgressRequest {
        method: http::Method::GET,
        uri: uri.parse().unwrap(),
        headers: http::HeaderMap::new(),
        body: Bytes::new(),
    }
}

async fn send(e: &Arc<dyn Egress>, req: EgressRequest) -> Result<EgressResponse, EgressError> {
    guard(e.send(req)).await
}

fn denied(r: Result<EgressResponse, EgressError>) -> DenyReason {
    match r {
        Err(EgressError::Denied { reason, .. }) => reason,
        other => panic!("expected Denied, got {other:?}"),
    }
}

fn localhost(port: u16) -> (Arc<dyn Egress>, Arc<AtomicUsize>) {
    let (r, lookups) = table(vec![]);
    (
        egress_with(&format!("localhost:{port}"), r, NetTimeouts::default()),
        lookups,
    )
}

#[tokio::test]
async fn allowed_localhost_without_lookup() {
    let s = serve().await;
    let (e, lookups) = localhost(s.port);
    let r = send(&e, get(&format!("http://localhost:{}/", s.port)))
        .await
        .unwrap();
    assert_eq!(r.status, 200);
    assert_eq!(&r.body[..], b"hello");
    assert_eq!(lookups.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn trailing_dot_is_normalised_once() {
    let s = serve().await;
    let (e, _) = localhost(s.port);
    let ok = send(&e, get(&format!("http://localhost.:{}/echo", s.port)))
        .await
        .unwrap();
    // The Host header is the normalised host.
    assert_eq!(
        std::str::from_utf8(&ok.body).unwrap(),
        format!("host=localhost:{};proxy_auth=false;body=0", s.port)
    );
    let r = send(&e, get(&format!("http://localhost..:{}/", s.port))).await;
    assert_eq!(denied(r), DenyReason::BadHost);
}

#[tokio::test]
async fn other_host_or_port_is_not_granted_and_never_resolved() {
    let s = serve().await;
    let (r, lookups) = table(vec![]);
    let e = egress_with(&format!("localhost:{}", s.port), r, NetTimeouts::default());
    for uri in [
        format!("http://localhost:{}/", s.port + 1),
        format!("http://other.example.com:{}/", s.port),
        format!("http://localhost.evil.com:{}/", s.port),
        "https://localhost/".to_owned(),
    ] {
        assert_eq!(
            denied(send(&e, get(&uri)).await),
            DenyReason::HostNotGranted,
            "{uri}"
        );
    }
    assert_eq!(lookups.load(Ordering::SeqCst), 0);
    assert_eq!(s.hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn ip_literals_userinfo_and_schemes_never_connect() {
    let s = serve().await;
    let (e, _) = localhost(s.port);
    for uri in [
        format!("http://127.0.0.1:{}/", s.port),
        format!("http://[::1]:{}/", s.port),
        format!("http://[::ffff:127.0.0.1]:{}/", s.port),
        format!("http://2130706433:{}/", s.port),
        format!("http://0x7f.1:{}/", s.port),
        format!("http://user:pw@localhost:{}/", s.port),
    ] {
        assert_eq!(
            denied(send(&e, get(&uri)).await),
            DenyReason::BadHost,
            "{uri}"
        );
    }
    assert_eq!(
        denied(send(&e, get(&format!("ftp://localhost:{}/", s.port))).await),
        DenyReason::BadScheme
    );
    assert_eq!(s.hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn no_capability_is_denied() {
    let id: PluginId = "demo".parse().unwrap();
    let e = NetEgressFactory::new().for_plugin(&id, &CapabilitySet::new());
    assert_eq!(
        denied(send(&e, get("https://example.com/")).await),
        DenyReason::NoCapability
    );
}

#[tokio::test]
async fn redirect_is_returned_not_followed() {
    let s = serve().await;
    let (e, _) = localhost(s.port);
    let r = send(&e, get(&format!("http://localhost:{}/redirect", s.port)))
        .await
        .unwrap();
    assert_eq!(r.status, 302);
    assert_eq!(r.headers["location"], "http://denied.example.com/");
    assert_eq!(s.hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn names_resolving_to_forbidden_addresses_are_refused_without_connecting() {
    let s = serve().await;
    let lo: IpAddr = "127.0.0.1".parse().unwrap();
    let public: IpAddr = "8.8.8.8".parse().unwrap();
    let (r, lookups) = table(vec![
        ("rebind.example.com", vec![lo]),
        ("mixed.example.com", vec![public, lo]),
        ("127.0.0.1.nip.io", vec![lo]),
        ("lan.example.com", vec!["10.0.0.5".parse().unwrap()]),
        ("meta.example.com", vec!["169.254.169.254".parse().unwrap()]),
        (
            "mapped.example.com",
            vec!["::ffff:127.0.0.1".parse().unwrap()],
        ),
        ("empty.example.com", vec![]),
    ]);
    for (name, reason) in [
        (
            "rebind.example.com",
            DenyReason::ForbiddenAddress { addr: lo },
        ),
        (
            "mixed.example.com",
            DenyReason::ForbiddenAddress { addr: lo },
        ),
        (
            "127.0.0.1.nip.io",
            DenyReason::ForbiddenAddress { addr: lo },
        ),
        (
            "lan.example.com",
            DenyReason::ForbiddenAddress {
                addr: "10.0.0.5".parse().unwrap(),
            },
        ),
        (
            "meta.example.com",
            DenyReason::ForbiddenAddress {
                addr: "169.254.169.254".parse().unwrap(),
            },
        ),
        (
            "mapped.example.com",
            DenyReason::ForbiddenAddress {
                addr: "::ffff:127.0.0.1".parse().unwrap(),
            },
        ),
        ("empty.example.com", DenyReason::NoAddress),
    ] {
        let e = egress_with(
            &format!("{name}:{}", s.port),
            r.clone(),
            NetTimeouts::default(),
        );
        let uri = format!("http://{name}:{}/", s.port);
        assert_eq!(denied(send(&e, get(&uri)).await), reason, "{name}");
    }
    assert_eq!(lookups.load(Ordering::SeqCst), 7);
    assert_eq!(s.hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn failed_lookup_is_a_dns_error() {
    let (r, _) = table(vec![]);
    let e = egress_with("nx.example.com:9", r, NetTimeouts::default());
    match send(&e, get("http://nx.example.com:9/")).await {
        Err(EgressError::Dns { host }) => assert_eq!(host, "nx.example.com"),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn connection_refused_is_a_connect_error() {
    // Bind then drop to get a port nobody listens on.
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    drop(l);
    let (e, _) = localhost(port);
    match send(&e, get(&format!("http://localhost:{port}/"))).await {
        Err(EgressError::Connect { message }) => {
            assert!(!message.contains("http://"), "{message}");
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn guest_host_header_is_dropped_and_proxy_credentials_never_forwarded() {
    let s = serve().await;
    let (e, _) = localhost(s.port);
    let mut req = get(&format!("http://localhost:{}/echo", s.port));
    req.headers
        .insert("host", "evil.example.com".parse().unwrap());
    req.headers
        .insert("proxy-authorization", "Basic eDp5".parse().unwrap());
    req.body = Bytes::from_static(b"12345");
    req.method = http::Method::POST;
    let r = send(&e, req).await.unwrap();
    assert_eq!(
        std::str::from_utf8(&r.body).unwrap(),
        format!("host=localhost:{};proxy_auth=false;body=5", s.port)
    );
}

#[tokio::test]
async fn response_cap_is_enforced() {
    let s = serve().await;
    let (e, _) = localhost(s.port);
    let big = send(&e, get(&format!("http://localhost:{}/big", s.port))).await;
    assert_eq!(
        big.unwrap_err(),
        EgressError::TooLarge {
            max: MAX_RESPONSE_BODY
        }
    );
    let max = send(&e, get(&format!("http://localhost:{}/max", s.port)))
        .await
        .unwrap();
    assert_eq!(max.body.len(), MAX_RESPONSE_BODY);
}

#[tokio::test]
async fn request_cap_is_enforced_before_connecting() {
    let s = serve().await;
    let (e, _) = localhost(s.port);
    let mut req = get(&format!("http://localhost:{}/echo", s.port));
    req.method = http::Method::POST;
    req.body = Bytes::from(vec![0u8; MAX_REQUEST_BODY + 1]);
    assert_eq!(
        send(&e, req).await.unwrap_err(),
        EgressError::TooLarge {
            max: MAX_REQUEST_BODY
        }
    );
    assert_eq!(s.hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn slow_server_times_out() {
    let s = serve().await;
    let (r, _) = table(vec![]);
    let e = egress_with(
        &format!("localhost:{}", s.port),
        r,
        NetTimeouts {
            connect: Duration::from_secs(5),
            total: Duration::from_millis(300),
        },
    );
    match send(&e, get(&format!("http://localhost:{}/slow", s.port))).await {
        Err(EgressError::Connect { message }) => assert_eq!(message, "timed out"),
        other => panic!("{other:?}"),
    }
}
