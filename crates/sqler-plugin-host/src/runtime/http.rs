// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The wasi-http send hook: every guest request goes through the `Egress` of that plugin, never to a socket.

use std::sync::Arc;

use bytes::BytesMut;
use http_body_util::{BodyExt, Full};
use wasmtime_wasi_http::{Error, RequestOptions, WasiBody, WasiHttpHooks};

use crate::egress::{Egress, EgressError, EgressRequest, MAX_REQUEST_BODY};

/// Routes `wasi:http/outgoing-handler` to an [`Egress`].
pub(crate) struct EgressHooks {
    egress: Arc<dyn Egress>,
}

impl EgressHooks {
    pub(crate) fn new(egress: Arc<dyn Egress>) -> Self {
        Self { egress }
    }
}

type Done = Box<dyn Future<Output = Result<(), Error>> + Send>;

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
    ) -> Box<dyn Future<Output = Result<(http::Response<WasiBody>, Done), Error>> + Send> {
        let egress = Arc::clone(&self.egress);
        Box::new(async move {
            let (parts, mut body) = request.into_parts();
            let mut buf = BytesMut::new();
            while let Some(frame) = body.frame().await {
                if let Ok(data) = frame?.into_data() {
                    if buf.len().saturating_add(data.len()) > MAX_REQUEST_BODY {
                        return Err(Error::HttpRequestBodySize(Some(
                            u64::try_from(buf.len() + data.len()).unwrap_or(u64::MAX),
                        )));
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
            let body: WasiBody = Full::new(response.body)
                .map_err(|never| match never {})
                .boxed_unsync();
            let mut out = http::Response::new(body);
            *out.status_mut() = response.status;
            *out.headers_mut() = response.headers;
            let done: Done = Box::new(async { Ok(()) });
            Ok((out, done))
        })
    }
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
    use crate::egress::DenyReason;

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
}
