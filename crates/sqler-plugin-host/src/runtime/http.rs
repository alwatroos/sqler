// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The wasi-http send hook: every guest request goes through the `Egress` of that plugin, never to a socket.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use bytes::BytesMut;
use http_body_util::{BodyExt, Full};
use wasmtime_wasi_http::{Error, RequestOptions, WasiBody, WasiHttpHooks};

use crate::egress::{
    EGRESS_MAX_BYTES_GLOBAL, EGRESS_MAX_BYTES_PER_PLUGIN, EGRESS_MAX_REQUESTS_GLOBAL,
    EGRESS_MAX_REQUESTS_PER_PLUGIN, Egress, EgressError, EgressRequest, MAX_REQUEST_BODY,
};

/// A counter pair: requests in flight and bytes held, each with a maximum. Lock-free, so taking and giving back never
/// waits.
pub(crate) struct Budget {
    requests: AtomicUsize,
    bytes: AtomicUsize,
    max_requests: usize,
    max_bytes: usize,
}

impl Budget {
    pub(crate) fn new(max_requests: usize, max_bytes: usize) -> Arc<Self> {
        Arc::new(Self {
            requests: AtomicUsize::new(0),
            bytes: AtomicUsize::new(0),
            max_requests,
            max_bytes,
        })
    }

    /// The default budget of one plugin.
    pub(crate) fn per_plugin() -> Arc<Self> {
        Self::new(EGRESS_MAX_REQUESTS_PER_PLUGIN, EGRESS_MAX_BYTES_PER_PLUGIN)
    }

    /// The default budget shared by every plugin of a host.
    pub(crate) fn global() -> Arc<Self> {
        Self::new(EGRESS_MAX_REQUESTS_GLOBAL, EGRESS_MAX_BYTES_GLOBAL)
    }

    fn take(counter: &AtomicUsize, n: usize, max: usize) -> bool {
        counter
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |cur| {
                cur.checked_add(n).filter(|total| *total <= max)
            })
            .is_ok()
    }

    fn give(counter: &AtomicUsize, n: usize) {
        counter.fetch_sub(n, Ordering::SeqCst);
    }
}

/// The plugin's budget and the host-wide one. A request needs room in both.
#[derive(Clone)]
pub(crate) struct Budgets {
    pub plugin: Arc<Budget>,
    pub global: Arc<Budget>,
}

/// What one request holds against the budgets: a request slot while it is in flight, and bytes (the buffered request
/// body, then the response body) until the response body is dropped. Everything is given back on drop.
pub(crate) struct Lease {
    budgets: Budgets,
    slot: bool,
    bytes: usize,
}

impl Lease {
    /// Takes a request slot in the plugin's budget and the global one, or `None` when either is full.
    fn open(budgets: &Budgets) -> Option<Self> {
        if !Budget::take(&budgets.plugin.requests, 1, budgets.plugin.max_requests) {
            return None;
        }
        if !Budget::take(&budgets.global.requests, 1, budgets.global.max_requests) {
            Budget::give(&budgets.plugin.requests, 1);
            return None;
        }
        Some(Self {
            budgets: budgets.clone(),
            slot: true,
            bytes: 0,
        })
    }

    /// Holds `n` more bytes; `false` (nothing taken) when either budget would be exceeded.
    fn charge(&mut self, n: usize) -> bool {
        let b = &self.budgets;
        if !Budget::take(&b.plugin.bytes, n, b.plugin.max_bytes) {
            return false;
        }
        if !Budget::take(&b.global.bytes, n, b.global.max_bytes) {
            Budget::give(&b.plugin.bytes, n);
            return false;
        }
        self.bytes += n;
        true
    }

    /// Gives back the request slot and the bytes held so far, then holds `n` bytes (the response). `false` when the
    /// response does not fit; the lease then holds nothing.
    fn settle(&mut self, n: usize) -> bool {
        self.release_slot();
        self.release_bytes();
        self.charge(n)
    }

    fn release_slot(&mut self) {
        if std::mem::take(&mut self.slot) {
            Budget::give(&self.budgets.plugin.requests, 1);
            Budget::give(&self.budgets.global.requests, 1);
        }
    }

    fn release_bytes(&mut self) {
        let n = std::mem::take(&mut self.bytes);
        if n > 0 {
            Budget::give(&self.budgets.plugin.bytes, n);
            Budget::give(&self.budgets.global.bytes, n);
        }
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.release_slot();
        self.release_bytes();
    }
}

/// Routes `wasi:http/outgoing-handler` to an [`Egress`], within the plugin's and the host's request and byte budgets:
/// a request over either is refused at once with `connection-limit-reached`, never queued.
pub(crate) struct EgressHooks {
    egress: Arc<dyn Egress>,
    budgets: Budgets,
}

impl EgressHooks {
    pub(crate) fn new(egress: Arc<dyn Egress>, budgets: Budgets) -> Self {
        Self { egress, budgets }
    }
}

type Done = Box<dyn Future<Output = Result<(), Error>> + Send>;

type Sent = Result<(http::Response<WasiBody>, Done), Error>;

impl WasiHttpHooks for EgressHooks {
    // The egress derives the `host` header from the URI it vets, so the guest's authority is the only source.
    fn set_host_header(&mut self) -> bool {
        false
    }

    fn send_request(
        &mut self,
        request: http::Request<WasiBody>,
        _options: Option<RequestOptions>,
        _fut: Done,
    ) -> Box<dyn Future<Output = Sent> + Send> {
        // Taken here, not when the future is first polled: the slot counts from the moment the guest asks.
        let Some(lease) = Lease::open(&self.budgets) else {
            tracing::warn!("egress refused: too many requests in flight");
            return Box::new(async { Err(Error::ConnectionLimitReached) });
        };
        let egress = Arc::clone(&self.egress);
        Box::new(send(egress, request, lease))
    }
}

async fn send(egress: Arc<dyn Egress>, request: http::Request<WasiBody>, mut lease: Lease) -> Sent {
    let (parts, mut body) = request.into_parts();
    let mut buf = BytesMut::new();
    while let Some(frame) = body.frame().await {
        if let Ok(data) = frame?.into_data() {
            if buf.len().saturating_add(data.len()) > MAX_REQUEST_BODY {
                return Err(Error::HttpRequestBodySize(Some(
                    u64::try_from(buf.len() + data.len()).unwrap_or(u64::MAX),
                )));
            }
            if !lease.charge(data.len()) {
                tracing::warn!("egress refused: request bytes over the budget");
                return Err(Error::ConnectionLimitReached);
            }
            buf.extend_from_slice(&data);
        }
    }
    let response = egress
        .send(EgressRequest {
            method: parts.method,
            uri: parts.uri,
            headers: parts.headers,
            body: buf.freeze(),
        })
        .await
        .map_err(|e| map_error(&e))?;
    // The request is over: its slot and body bytes go back, the response bytes are held until the guest drops the
    // body, so unread responses cannot pile up.
    if !lease.settle(response.body.len()) {
        tracing::warn!("egress refused: response bytes over the budget");
        return Err(Error::ConnectionLimitReached);
    }
    let body: WasiBody = Full::new(response.body)
        .map_frame(move |frame| {
            let _held = &lease;
            frame
        })
        .map_err(|never| match never {})
        .boxed_unsync();
    let mut out = http::Response::new(body);
    *out.status_mut() = response.status;
    *out.headers_mut() = response.headers;
    let done: Done = Box::new(async { Ok(()) });
    Ok((out, done))
}

/// The wasi-http error code a guest sees for an egress failure. Detail stays on the host side.
pub(crate) fn map_error(e: &EgressError) -> Error {
    tracing::debug!(error = ?e, "egress failed");
    match e {
        EgressError::Denied { .. } => Error::HttpRequestDenied,
        EgressError::Dns { .. } => Error::DnsError {
            rcode: None,
            info_code: None,
        },
        EgressError::Connect { .. } => Error::ConnectionRefused,
        EgressError::Tls { .. } => Error::TlsProtocolError,
        EgressError::TooLarge { .. } => Error::HttpResponseBodySize(None),
        _ => Error::HttpProtocolError,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::egress::{DenyReason, EgressFuture, EgressResponse};
    use bytes::Bytes;
    use std::time::Duration;
    use tokio::sync::Notify;

    #[test]
    fn egress_errors_map_to_wasi_http_codes() {
        let denied = EgressError::Denied {
            host: "h".into(),
            reason: DenyReason::HostNotGranted,
        };
        assert!(matches!(map_error(&denied), Error::HttpRequestDenied));
        assert!(matches!(
            map_error(&EgressError::Dns { host: "h".into() }),
            Error::DnsError { .. }
        ));
        assert!(matches!(
            map_error(&EgressError::Connect {
                message: String::new()
            }),
            Error::ConnectionRefused
        ));
        assert!(matches!(
            map_error(&EgressError::Tls {
                message: String::new()
            }),
            Error::TlsProtocolError
        ));
        assert!(matches!(
            map_error(&EgressError::TooLarge { max: 1 }),
            Error::HttpResponseBodySize(_)
        ));
        assert!(matches!(
            map_error(&EgressError::Protocol {
                message: String::new()
            }),
            Error::HttpProtocolError
        ));
    }

    /// An egress that holds every request open until `release` and answers with `reply` bytes.
    struct Held {
        release: Arc<Notify>,
        open: Arc<AtomicUsize>,
        reply: usize,
    }

    impl Egress for Held {
        fn send(&self, _req: EgressRequest) -> EgressFuture {
            let (release, open, reply) = (
                Arc::clone(&self.release),
                Arc::clone(&self.open),
                self.reply,
            );
            Box::pin(async move {
                open.fetch_add(1, Ordering::SeqCst);
                release.notified().await;
                open.fetch_sub(1, Ordering::SeqCst);
                Ok(EgressResponse {
                    status: http::StatusCode::OK,
                    headers: http::HeaderMap::new(),
                    body: Bytes::from(vec![0; reply]),
                })
            })
        }
    }

    struct Rig {
        hooks: EgressHooks,
        release: Arc<Notify>,
        open: Arc<AtomicUsize>,
    }

    fn rig(budgets: &Budgets, reply: usize) -> Rig {
        let release = Arc::new(Notify::new());
        let open = Arc::new(AtomicUsize::new(0));
        let egress = Held {
            release: Arc::clone(&release),
            open: Arc::clone(&open),
            reply,
        };
        Rig {
            hooks: EgressHooks::new(Arc::new(egress), budgets.clone()),
            release,
            open,
        }
    }

    fn request(body: usize) -> http::Request<WasiBody> {
        let b: WasiBody = Full::new(Bytes::from(vec![1; body]))
            .map_err(|never| match never {})
            .boxed_unsync();
        http::Request::new(b)
    }

    fn budgets(requests: usize, bytes: usize, global: (usize, usize)) -> Budgets {
        Budgets {
            plugin: Budget::new(requests, bytes),
            global: Budget::new(global.0, global.1),
        }
    }

    type Task = tokio::task::JoinHandle<Sent>;

    fn start(rig: &mut Rig, body: usize) -> Task {
        let fut = rig
            .hooks
            .send_request(request(body), None, Box::new(async { Ok(()) }));
        tokio::spawn(Box::into_pin(fut))
    }

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
    }

    /// Lets every ready task run; the hooks never wait on anything but the held egress.
    async fn settle() {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    fn refused(r: &Result<Sent, tokio::task::JoinError>) -> bool {
        matches!(r, Ok(Err(Error::ConnectionLimitReached)))
    }

    #[test]
    fn requests_over_the_plugin_cap_are_refused_not_queued() {
        rt().block_on(async {
            let mut r = rig(&budgets(2, 1 << 20, (100, 1 << 30)), 0);
            let held = [start(&mut r, 1), start(&mut r, 1)];
            let third = start(&mut r, 1);
            let fourth = start(&mut r, 1);
            settle().await;
            // The over-cap ones are already answered while the first two are still open.
            assert!(third.is_finished() && fourth.is_finished());
            assert!(refused(&third.await));
            assert!(refused(&fourth.await));
            assert_eq!(r.open.load(Ordering::SeqCst), 2);
            assert!(held.iter().all(|t| !t.is_finished()));
            r.release.notify_waiters();
            for t in held {
                assert!(matches!(t.await, Ok(Ok(_))));
            }
            // The slots are back.
            let again = start(&mut r, 1);
            settle().await;
            r.release.notify_waiters();
            assert!(matches!(again.await, Ok(Ok(_))));
        });
    }

    #[test]
    fn the_global_cap_spans_plugins() {
        rt().block_on(async {
            let global = Budget::new(3, 1 << 30);
            let b = |g: &Arc<Budget>| Budgets {
                plugin: Budget::new(2, 1 << 20),
                global: Arc::clone(g),
            };
            let (mut a, mut c) = (rig(&b(&global), 0), rig(&b(&global), 0));
            let held = [start(&mut a, 1), start(&mut a, 1), start(&mut c, 1)];
            let over = start(&mut c, 1);
            settle().await;
            assert!(refused(&over.await));
            a.release.notify_waiters();
            c.release.notify_waiters();
            for t in held {
                assert!(matches!(t.await, Ok(Ok(_))));
            }
        });
    }

    #[test]
    fn buffered_request_bytes_count_against_the_byte_budget() {
        rt().block_on(async {
            let mut r = rig(&budgets(8, 15, (100, 1 << 30)), 0);
            let first = start(&mut r, 10);
            settle().await;
            let second = start(&mut r, 10);
            settle().await;
            assert!(refused(&second.await));
            r.release.notify_waiters();
            assert!(matches!(first.await, Ok(Ok(_))));
            // Given back once the request is over.
            let third = start(&mut r, 10);
            settle().await;
            r.release.notify_waiters();
            assert!(matches!(third.await, Ok(Ok(_))));
        });
    }

    #[test]
    fn unread_response_bytes_stay_charged_until_the_body_is_dropped() {
        rt().block_on(async {
            let b = budgets(8, 20, (100, 1 << 30));
            let mut r = rig(&b, 16);
            let first = start(&mut r, 0);
            settle().await;
            r.release.notify_waiters();
            let Ok(Ok((response, _))) = first.await else {
                panic!("first request failed")
            };
            assert_eq!(b.plugin.bytes.load(Ordering::SeqCst), 16);
            assert_eq!(b.plugin.requests.load(Ordering::SeqCst), 0);
            let second = start(&mut r, 0);
            settle().await;
            r.release.notify_waiters();
            assert!(refused(&second.await));
            drop(response);
            assert_eq!(b.plugin.bytes.load(Ordering::SeqCst), 0);
            assert_eq!(b.global.bytes.load(Ordering::SeqCst), 0);
            let third = start(&mut r, 0);
            settle().await;
            r.release.notify_waiters();
            assert!(matches!(third.await, Ok(Ok(_))));
        });
    }

    #[test]
    fn a_dropped_request_gives_its_slot_back() {
        rt().block_on(async {
            let b = budgets(1, 1 << 20, (100, 1 << 30));
            let mut r = rig(&b, 0);
            let first = start(&mut r, 5);
            settle().await;
            assert_eq!(b.plugin.requests.load(Ordering::SeqCst), 1);
            first.abort();
            let _ = first.await;
            assert_eq!(b.plugin.requests.load(Ordering::SeqCst), 0);
            assert_eq!(b.plugin.bytes.load(Ordering::SeqCst), 0);
            assert_eq!(b.global.requests.load(Ordering::SeqCst), 0);
        });
    }
}
