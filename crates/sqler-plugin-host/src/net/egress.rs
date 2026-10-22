// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The reqwest-backed [`Egress`].

use std::error::Error as StdError;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use http::header::{self, HeaderName};
use sqler_plugin_api::{CapabilitySet, PluginId};

use super::host::{Target, check_host};
use super::resolver::{Resolver, SystemResolver, VetError, VettedResolver};
use crate::egress::{
    DenyReason, Egress, EgressError, EgressFactory, EgressFuture, EgressRequest, EgressResponse,
    MAX_REQUEST_BODY, MAX_RESPONSE_BODY,
};

/// Time limits of one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetTimeouts {
    /// Name lookup plus TCP and TLS connect.
    pub connect: Duration,
    /// The whole request, body included.
    pub total: Duration,
}

impl Default for NetTimeouts {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(10),
            total: Duration::from_secs(30),
        }
    }
}

/// Headers the host owns or that only make sense per hop; a guest cannot set them and a peer's are not forwarded.
const HOP_BY_HOP: [HeaderName; 8] = [
    header::HOST,
    header::CONNECTION,
    header::PROXY_AUTHENTICATE,
    header::PROXY_AUTHORIZATION,
    header::TE,
    header::TRAILER,
    header::TRANSFER_ENCODING,
    header::UPGRADE,
];

fn sanitize(headers: &mut http::HeaderMap) {
    for name in &HOP_BY_HOP {
        headers.remove(name);
    }
    headers.remove("keep-alive");
    headers.remove("proxy-connection");
}

/// Builds a [`NetEgress`] per plugin on one shared client. `new` does no I/O: the client is built on first use.
pub struct NetEgressFactory {
    shared: Arc<Shared>,
}

struct Shared {
    resolver: Arc<dyn Resolver>,
    timeouts: NetTimeouts,
    client: OnceLock<Result<reqwest::Client, String>>,
}

impl Shared {
    fn client(&self) -> Result<&reqwest::Client, EgressError> {
        self.client
            .get_or_init(|| {
                reqwest::Client::builder()
                    .no_proxy()
                    .redirect(reqwest::redirect::Policy::none())
                    .dns_resolver(Arc::new(VettedResolver::new(self.resolver.clone())))
                    .connect_timeout(self.timeouts.connect)
                    .timeout(self.timeouts.total)
                    .build()
                    .map_err(|e| e.without_url().to_string())
            })
            .as_ref()
            .map_err(|message| EgressError::Connect {
                message: message.clone(),
            })
    }
}

impl Default for NetEgressFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl NetEgressFactory {
    /// A factory over the system resolver and the default timeouts.
    #[must_use]
    pub fn new() -> Self {
        Self::with_parts(Arc::new(SystemResolver), NetTimeouts::default())
    }

    /// A factory over `resolver` and `timeouts` (tests inject both).
    #[must_use]
    pub fn with_parts(resolver: Arc<dyn Resolver>, timeouts: NetTimeouts) -> Self {
        Self {
            shared: Arc::new(Shared {
                resolver,
                timeouts,
                client: OnceLock::new(),
            }),
        }
    }
}

impl EgressFactory for NetEgressFactory {
    fn for_plugin(&self, plugin: &PluginId, effective: &CapabilitySet) -> Arc<dyn Egress> {
        Arc::new(NetEgress {
            plugin: plugin.clone(),
            effective: effective.clone(),
            shared: self.shared.clone(),
        })
    }
}

/// The egress of one plugin: enforces its `net.http` grant, vets every resolved address, buffers bodies.
pub struct NetEgress {
    plugin: PluginId,
    effective: CapabilitySet,
    shared: Arc<Shared>,
}

impl Egress for NetEgress {
    fn send(&self, req: EgressRequest) -> EgressFuture {
        let plugin = self.plugin.clone();
        let effective = self.effective.clone();
        let shared = self.shared.clone();
        Box::pin(async move {
            let result = send(&shared, &effective, req).await;
            match &result {
                // Never the URL: it may carry a path, a query or credentials. Errors name the host only.
                Ok(resp) => {
                    tracing::debug!(plugin = %plugin, status = resp.status.as_u16(), "plugin http request");
                }
                Err(e) => {
                    tracing::debug!(plugin = %plugin, error = %e, "plugin http request failed");
                }
            }
            result
        })
    }
}

async fn send(
    shared: &Shared,
    effective: &CapabilitySet,
    req: EgressRequest,
) -> Result<EgressResponse, EgressError> {
    let target = check_host(effective, &req.uri)?;
    if req.body.len() > MAX_REQUEST_BODY {
        return Err(EgressError::TooLarge {
            max: MAX_REQUEST_BODY,
        });
    }
    let client = shared.client()?;
    let url = build_url(&target, &req.uri);
    let mut headers = req.headers;
    sanitize(&mut headers);
    headers.remove(header::CONTENT_LENGTH);
    let mut resp = client
        .request(req.method, url)
        .headers(headers)
        .body(req.body)
        .send()
        .await
        .map_err(|e| map_error(&target.host, &e))?;
    if resp
        .content_length()
        .is_some_and(|n| n > MAX_RESPONSE_BODY as u64)
    {
        return Err(EgressError::TooLarge {
            max: MAX_RESPONSE_BODY,
        });
    }
    let status = resp.status();
    let mut out_headers = resp.headers().clone();
    sanitize(&mut out_headers);
    let mut body = BytesMut::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| map_error(&target.host, &e))?
    {
        if body.len() + chunk.len() > MAX_RESPONSE_BODY {
            return Err(EgressError::TooLarge {
                max: MAX_RESPONSE_BODY,
            });
        }
        body.extend_from_slice(&chunk);
    }
    Ok(EgressResponse {
        status,
        headers: out_headers,
        body: Bytes::from(body),
    })
}

/// `scheme://host:port/path?query` with the normalised host (no userinfo, no trailing dot, no fragment).
fn build_url(target: &Target, uri: &http::Uri) -> String {
    let pq = uri.path_and_query().map_or("/", |p| p.as_str());
    format!("{}://{}:{}{}", target.scheme, target.host, target.port, pq)
}

fn map_error(host: &str, err: &reqwest::Error) -> EgressError {
    let denied = |reason| EgressError::Denied {
        host: host.to_owned(),
        reason,
    };
    // The reqwest error carries the URL in its Display; walk the sources instead.
    let mut source: Option<&(dyn StdError + 'static)> = Some(err);
    let mut text = String::new();
    while let Some(e) = source {
        if let Some(v) = e.downcast_ref::<VetError>() {
            return match v {
                VetError::Forbidden(addr) => denied(DenyReason::ForbiddenAddress { addr: *addr }),
                VetError::NoAddress => denied(DenyReason::NoAddress),
                VetError::Lookup => EgressError::Dns {
                    host: host.to_owned(),
                },
            };
        }
        if !e.is::<reqwest::Error>() {
            text.push_str(&e.to_string());
            text.push_str(": ");
        }
        source = e.source();
    }
    let text = text.trim_end_matches(": ").to_owned();
    if err.is_timeout() {
        return EgressError::Connect {
            message: "timed out".into(),
        };
    }
    let lower = text.to_ascii_lowercase();
    if lower.contains("certificate")
        || lower.contains("handshake")
        || lower.contains("tls")
        || lower.contains("invalid peer")
    {
        return EgressError::Tls { message: text };
    }
    if err.is_connect() {
        return EgressError::Connect {
            message: if text.is_empty() {
                "connection failed".into()
            } else {
                text
            },
        };
    }
    EgressError::Protocol {
        message: if text.is_empty() {
            "request failed".into()
        } else {
            text
        },
    }
}
