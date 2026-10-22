// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Outgoing HTTP from a plugin: the request/response types and the traits the runtime calls. Bodies are buffered;
//! streaming would be an additive change.

use std::future::Future;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::Arc;

use bytes::Bytes;
use sqler_plugin_api::{CapabilitySet, PluginId};
use thiserror::Error;

/// Largest request body a plugin may send.
pub const MAX_REQUEST_BODY: usize = 8 << 20;
/// Largest response body handed to a plugin.
pub const MAX_RESPONSE_BODY: usize = 16 << 20;

/// Requests one plugin may have in flight at once (plus unread responses it still holds); more are refused with
/// `connection-limit-reached`, not queued.
pub const EGRESS_MAX_REQUESTS_PER_PLUGIN: usize = 4;
/// Bytes one plugin may hold in the host at once: request bodies being sent and response bodies it has not dropped.
pub const EGRESS_MAX_BYTES_PER_PLUGIN: usize = 32 << 20;
/// Requests in flight across every plugin of one host.
pub const EGRESS_MAX_REQUESTS_GLOBAL: usize = 16;
/// Bytes held across every plugin of one host.
pub const EGRESS_MAX_BYTES_GLOBAL: usize = 96 << 20;

/// One outgoing request.
#[derive(Debug, Clone)]
pub struct EgressRequest {
    /// Method.
    pub method: http::Method,
    /// Absolute URI.
    pub uri: http::Uri,
    /// Headers as the guest set them.
    pub headers: http::HeaderMap,
    /// Body.
    pub body: Bytes,
}

/// The answer.
#[derive(Debug, Clone)]
pub struct EgressResponse {
    /// Status.
    pub status: http::StatusCode,
    /// Headers.
    pub headers: http::HeaderMap,
    /// Body, at most [`MAX_RESPONSE_BODY`].
    pub body: Bytes,
}

/// The future of one request.
pub type EgressFuture = Pin<Box<dyn Future<Output = Result<EgressResponse, EgressError>> + Send>>;

/// Sends requests on behalf of one plugin. Implementations enforce the plugin's `net.http` grant.
pub trait Egress: Send + Sync {
    /// Sends `req`.
    fn send(&self, req: EgressRequest) -> EgressFuture;
}

/// Builds the [`Egress`] of a plugin from its effective capabilities.
pub trait EgressFactory: Send + Sync {
    /// The egress for `plugin` with `effective` capabilities.
    fn for_plugin(&self, plugin: &PluginId, effective: &CapabilitySet) -> Arc<dyn Egress>;
}

/// Why a request was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DenyReason {
    /// The plugin has no `net.http` capability.
    NoCapability,
    /// No `net.http` pattern covers the host and port.
    HostNotGranted,
    /// The host in the URI is malformed.
    BadHost,
    /// The host resolves to an address the sandbox never reaches (loopback, private, link-local, ...).
    ForbiddenAddress {
        /// The offending address.
        addr: IpAddr,
    },
    /// The host resolves to no address.
    NoAddress,
    /// The scheme is not `http` or `https`.
    BadScheme,
}

/// Why a request failed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum EgressError {
    /// Refused before any connection.
    #[error("request to `{host}` denied: {reason:?}")]
    Denied {
        /// The host, lowercased, punycode kept.
        host: String,
        /// Why.
        reason: DenyReason,
    },
    /// Name resolution failed.
    #[error("cannot resolve `{host}`")]
    Dns {
        /// The host.
        host: String,
    },
    /// The connection failed.
    #[error("connection failed: {message}")]
    Connect {
        /// The message.
        message: String,
    },
    /// The TLS handshake failed.
    #[error("TLS failed: {message}")]
    Tls {
        /// The message.
        message: String,
    },
    /// A body is over its cap.
    #[error("body is larger than {max} bytes")]
    TooLarge {
        /// The cap.
        max: usize,
    },
    /// The peer broke the HTTP protocol.
    #[error("HTTP protocol error: {message}")]
    Protocol {
        /// The message.
        message: String,
    },
}

/// Egress that refuses everything: the default when no network is configured.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoEgress;

impl Egress for NoEgress {
    fn send(&self, req: EgressRequest) -> EgressFuture {
        let host = req.uri.host().unwrap_or_default().to_ascii_lowercase();
        Box::pin(async move {
            Err(EgressError::Denied {
                host,
                reason: DenyReason::NoCapability,
            })
        })
    }
}

impl EgressFactory for NoEgress {
    fn for_plugin(&self, _plugin: &PluginId, _effective: &CapabilitySet) -> Arc<dyn Egress> {
        Arc::new(Self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::task::{Context, Poll, Waker};

    fn block_on<F: Future>(f: F) -> F::Output {
        let mut f = std::pin::pin!(f);
        let mut cx = Context::from_waker(Waker::noop());
        match f.as_mut().poll(&mut cx) {
            Poll::Ready(v) => v,
            Poll::Pending => unreachable!("NoEgress never waits"),
        }
    }

    #[test]
    fn no_egress_denies_everything() {
        let req = EgressRequest {
            method: http::Method::GET,
            uri: "https://Example.com/x".parse().unwrap(),
            headers: http::HeaderMap::new(),
            body: Bytes::new(),
        };
        let id: PluginId = "demo".parse().unwrap();
        let egress = NoEgress.for_plugin(&id, &CapabilitySet::new());
        assert_eq!(
            block_on(egress.send(req)).unwrap_err(),
            EgressError::Denied {
                host: "example.com".into(),
                reason: DenyReason::NoCapability
            }
        );
    }
}
