// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Maps HTTP statuses and `reqwest` errors to `AiError`.
//!
//! Every string is fixed or comes from the server's own error message, with
//! the API key replaced by `***` and the text cut to 200 characters. None of
//! them holds the URL, a header or the prompt.

use std::time::Duration;

use reqwest::Response;
use reqwest::header::HeaderMap;
use serde_json::Value;
use sqler_ai_api::AiError;

const MAX_ERROR_CHARS: usize = 200;
/// How much of an error body is read.
const MAX_ERROR_BODY: usize = 64 * 1024;

/// Cuts text to 200 characters.
pub(crate) fn truncate(text: &str) -> String {
    text.chars().take(MAX_ERROR_CHARS).collect()
}

/// Replaces every occurrence of the key, then cuts to 200 characters.
pub(crate) fn scrub(text: &str, key: Option<&str>) -> String {
    match key.filter(|k| !k.is_empty()) {
        Some(k) => truncate(&text.replace(k, "***")),
        None => truncate(text),
    }
}

/// The error text of a JSON error body: `error.message`, `error` as a string,
/// `message`, or `detail` as a string.
pub(crate) fn message_of(v: &Value) -> Option<&str> {
    let err = v.get("error");
    err.and_then(|e| e.get("message"))
        .and_then(Value::as_str)
        .or_else(|| err.and_then(Value::as_str))
        .or_else(|| v.get("message").and_then(Value::as_str))
        .or_else(|| v.get("detail").and_then(Value::as_str))
}

/// The error of an `error` member found in a stream payload.
pub(crate) fn stream_error(v: &Value, key: Option<&str>) -> AiError {
    let text = scrub(message_of(v).unwrap_or(""), key);
    if text.is_empty() {
        AiError::Provider("stream error".to_owned())
    } else {
        AiError::Provider(text)
    }
}

fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let text = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    // `try_from_secs_f64` rejects negative, NaN and out-of-range values
    // instead of panicking; the header comes from the server.
    if let Some(d) = text("retry-after-ms")
        .and_then(|v| v.trim().parse::<f64>().ok())
        .and_then(|ms| Duration::try_from_secs_f64(ms / 1000.0).ok())
    {
        return Some(d);
    }
    text("retry-after")
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
}

/// Maps a non-success status. `body` is the (capped) response body and `key`
/// the API key to scrub out of any server text.
pub(crate) fn from_status(
    code: u16,
    headers: &HeaderMap,
    body: &[u8],
    key: Option<&str>,
) -> AiError {
    let parsed: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let text = scrub(message_of(&parsed).unwrap_or(""), key);
    let error_code = parsed
        .get("error")
        .and_then(|e| e.get("code"))
        .and_then(Value::as_str)
        .unwrap_or("");
    match code {
        // OpenAI's 401 text quotes a masked key, so the server text is not used.
        401 | 403 => AiError::Auth(format!("HTTP {code}")),
        429 if error_code == "insufficient_quota" => AiError::Provider(format!("HTTP 429: {text}")),
        429 => AiError::RateLimited {
            retry_after: retry_after(headers),
        },
        400 if matches!(error_code, "unsupported_parameter" | "unsupported_value")
            || text.contains("does not support")
            || text.contains("not supported") =>
        {
            if text.is_empty() {
                AiError::Unsupported(format!("HTTP 400: {error_code}"))
            } else {
                AiError::Unsupported(text)
            }
        }
        _ if text.is_empty() => AiError::Provider(format!("HTTP {code}")),
        _ => AiError::Provider(format!("HTTP {code}: {text}")),
    }
}

/// Maps a transport error. The URL is removed first.
pub(crate) fn from_reqwest(e: reqwest::Error) -> AiError {
    let e = e.without_url();
    let msg = if e.is_timeout() {
        "timed out"
    } else if e.is_connect() {
        "cannot connect to the server"
    } else if e.is_body() || e.is_decode() {
        "response interrupted"
    } else {
        "request failed"
    };
    AiError::Network(msg.to_owned())
}

/// Reads at most `cap` bytes of a body.
pub(crate) async fn read_capped(resp: &mut Response, cap: usize) -> Result<Vec<u8>, AiError> {
    let mut out = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(from_reqwest)? {
        out.extend_from_slice(&chunk);
        if out.len() > cap {
            return Err(AiError::Protocol("response too large".to_owned()));
        }
    }
    Ok(out)
}

/// Builds the error of a response whose status is not a success.
pub(crate) async fn from_response(mut resp: Response, key: Option<&str>) -> AiError {
    let code = resp.status().as_u16();
    let headers = resp.headers().clone();
    let body = read_capped(&mut resp, MAX_ERROR_BODY)
        .await
        .unwrap_or_default();
    from_status(code, &headers, &body, key)
}

#[cfg(test)]
mod tests {
    use reqwest::header::{HeaderName, HeaderValue};

    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(v).unwrap(),
            );
        }
        h
    }

    fn st(code: u16, h: &[(&str, &str)], body: &str) -> AiError {
        from_status(code, &headers(h), body.as_bytes(), Some("sk-secret"))
    }

    #[test]
    fn auth_ignores_the_server_text() {
        let body =
            r#"{"error":{"message":"Incorrect API key provided: sk-sec****. You can find..."}}"#;
        assert_eq!(st(401, &[], body), AiError::Auth("HTTP 401".into()));
        assert_eq!(st(403, &[], "x"), AiError::Auth("HTTP 403".into()));
    }

    #[test]
    fn rate_limits() {
        assert_eq!(
            st(429, &[("retry-after", "3")], "{}"),
            AiError::RateLimited {
                retry_after: Some(Duration::from_secs(3))
            }
        );
        assert_eq!(
            st(
                429,
                &[("retry-after-ms", "250"), ("retry-after", "3")],
                "{}"
            ),
            AiError::RateLimited {
                retry_after: Some(Duration::from_millis(250))
            }
        );
        assert_eq!(
            st(429, &[("retry-after", "Wed, 21 Oct 2026 07:28:00 GMT")], ""),
            AiError::RateLimited { retry_after: None }
        );
        assert_eq!(
            st(
                429,
                &[],
                r#"{"error":{"message":"You exceeded your current quota","code":"insufficient_quota"}}"#
            ),
            AiError::Provider("HTTP 429: You exceeded your current quota".into())
        );
    }

    #[test]
    fn hostile_retry_after_ms_does_not_panic() {
        for bad in ["1e30", "-1", "NaN", "inf", "99999999999999999999999999"] {
            assert_eq!(
                st(429, &[("retry-after-ms", bad), ("retry-after", "3")], "{}"),
                AiError::RateLimited {
                    retry_after: Some(Duration::from_secs(3))
                },
                "{bad}"
            );
        }
    }

    #[test]
    fn unsupported_without_a_message_uses_the_code() {
        assert_eq!(
            st(400, &[], r#"{"error":{"code":"unsupported_parameter"}}"#),
            AiError::Unsupported("HTTP 400: unsupported_parameter".into())
        );
    }

    #[test]
    fn unsupported() {
        assert_eq!(
            st(
                400,
                &[],
                r#"{"error":{"message":"Unsupported value: 'temperature'","code":"unsupported_value"}}"#
            ),
            AiError::Unsupported("Unsupported value: 'temperature'".into())
        );
        assert_eq!(
            st(
                400,
                &[],
                r#"{"error":{"message":"model x does not support tools"}}"#
            ),
            AiError::Unsupported("model x does not support tools".into())
        );
        assert_eq!(
            st(400, &[], r#"{"error":"feature not supported here"}"#),
            AiError::Unsupported("feature not supported here".into())
        );
        assert_eq!(
            st(400, &[], r#"{"error":{"message":"bad"}}"#),
            AiError::Provider("HTTP 400: bad".into())
        );
    }

    #[test]
    fn body_shapes() {
        assert_eq!(
            st(404, &[], r#"{"error":{"message":"no such model"}}"#),
            AiError::Provider("HTTP 404: no such model".into())
        );
        assert_eq!(
            st(500, &[], r#"{"error":"flat"}"#),
            AiError::Provider("HTTP 500: flat".into())
        );
        assert_eq!(
            st(500, &[], r#"{"message":"top"}"#),
            AiError::Provider("HTTP 500: top".into())
        );
        assert_eq!(
            st(422, &[], r#"{"detail":"vllm style"}"#),
            AiError::Provider("HTTP 422: vllm style".into())
        );
        assert_eq!(st(500, &[], ""), AiError::Provider("HTTP 500".into()));
        assert_eq!(st(502, &[], "<html>"), AiError::Provider("HTTP 502".into()));
    }

    #[test]
    fn key_is_redacted_and_text_is_cut() {
        assert_eq!(
            st(500, &[], r#"{"error":"bad sk-secret and sk-secret"}"#),
            AiError::Provider("HTTP 500: bad *** and ***".into())
        );
        let long = format!(r#"{{"error":"{}"}}"#, "é".repeat(500));
        let AiError::Provider(m) = st(500, &[], &long) else {
            panic!("expected Provider");
        };
        assert_eq!(m.chars().count(), "HTTP 500: ".len() + 200);
    }

    #[test]
    fn stream_errors() {
        let v: Value =
            serde_json::from_str(r#"{"error":{"message":"overloaded sk-secret"}}"#).unwrap();
        assert_eq!(
            stream_error(&v, Some("sk-secret")),
            AiError::Provider("overloaded ***".into())
        );
        let v: Value = serde_json::from_str(r#"{"error":{}}"#).unwrap();
        assert_eq!(
            stream_error(&v, None),
            AiError::Provider("stream error".into())
        );
    }
}
