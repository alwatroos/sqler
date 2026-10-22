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
/// The longest `Retry-After` reported; a hostile header must not make
/// `Instant::now() + retry_after` overflow.
const MAX_RETRY_AFTER_SECS: u64 = 3600;

/// Cuts text to 200 characters.
pub(crate) fn truncate(text: &str) -> String {
    text.chars().take(MAX_ERROR_CHARS).collect()
}

/// Replaces every occurrence of the key, then cuts to 200 characters. The key
/// goes first, so a cut can never leave half of it in the text.
pub(crate) fn scrub(text: &str, key: Option<&str>) -> String {
    match key.filter(|k| !k.is_empty()) {
        Some(k) => truncate(&text.replace(k, "***")),
        None => truncate(text),
    }
}

/// `detail[0].msg` of a `FastAPI` validation error.
fn detail_msg(v: &Value) -> Option<&str> {
    v.as_array()?.first()?.get("msg")?.as_str()
}

/// The error text of a JSON error body, tried in this order: `message` as a
/// string; `message` as an object (`detail[0].msg`, else the object as compact
/// JSON); `error.message`; `error` as a string; `detail` as a string or
/// `detail[0].msg`.
pub(crate) fn message_of(v: &Value) -> Option<String> {
    if let Some(m) = v.get("message") {
        if let Some(s) = m.as_str() {
            return Some(s.to_owned());
        }
        if m.is_object() {
            return Some(
                m.get("detail")
                    .and_then(detail_msg)
                    .map_or_else(|| m.to_string(), str::to_owned),
            );
        }
    }
    let err = v.get("error");
    if let Some(s) = err.and_then(|e| e.get("message")).and_then(Value::as_str) {
        return Some(s.to_owned());
    }
    if let Some(s) = err.and_then(Value::as_str) {
        return Some(s.to_owned());
    }
    let detail = v.get("detail")?;
    detail
        .as_str()
        .or_else(|| detail_msg(detail))
        .map(str::to_owned)
}

/// The error of an error payload found in a stream.
pub(crate) fn stream_error(v: &Value, key: Option<&str>) -> AiError {
    let text = scrub(&message_of(v).unwrap_or_default(), key);
    if text.is_empty() {
        AiError::Provider("stream error".to_owned())
    } else {
        AiError::Provider(text)
    }
}

/// `Retry-After` as whole seconds, clamped to one hour. A date, a float, a
/// negative number or junk gives `None`. There is no float path, so nothing
/// can panic on a hostile header.
fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    headers
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(|n| Duration::from_secs(n.min(MAX_RETRY_AFTER_SECS)))
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
    let text = scrub(&message_of(&parsed).unwrap_or_default(), key);
    match code {
        // The server text of a 401 is not trusted to be free of credentials.
        401 | 403 => AiError::Auth(format!("HTTP {code}")),
        429 => AiError::RateLimited {
            retry_after: retry_after(headers),
        },
        400 | 422 if says_unsupported(&text) => AiError::Unsupported(text),
        _ if text.is_empty() => AiError::Provider(format!("HTTP {code}")),
        _ => AiError::Provider(format!("HTTP {code}: {text}")),
    }
}

fn says_unsupported(text: &str) -> bool {
    let lower = text.to_lowercase();
    ["not supported", "does not support", "not enabled"]
        .iter()
        .any(|p| lower.contains(p))
}

/// A `Protocol` error for JSON that does not parse. The text of a `serde_json`
/// error can quote the offending value, which is server data and might hold
/// the key, so only its category and position are kept.
pub(crate) fn malformed(what: &str, e: &serde_json::Error) -> AiError {
    use serde_json::error::Category;
    let kind = match e.classify() {
        Category::Io => "read error",
        Category::Syntax => "syntax error",
        Category::Data => "unexpected shape",
        Category::Eof => "unexpected end",
    };
    AiError::Protocol(format!(
        "malformed {what}: {kind} at line {} column {}",
        e.line(),
        e.column()
    ))
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

    const KEY: &str = "mst-secret";

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
        from_status(code, &headers(h), body.as_bytes(), Some(KEY))
    }

    fn secs(n: u64) -> AiError {
        AiError::RateLimited {
            retry_after: Some(Duration::from_secs(n)),
        }
    }

    #[test]
    fn auth_ignores_the_server_text() {
        assert_eq!(
            st(401, &[], r#"{"message":"Unauthorized"}"#),
            AiError::Auth("HTTP 401".into())
        );
        assert_eq!(
            st(403, &[], r#"{"message":"bad key mst-secret"}"#),
            AiError::Auth("HTTP 403".into())
        );
    }

    #[test]
    fn message_shapes_in_order() {
        let msg = |s: &str| message_of(&serde_json::from_str::<Value>(s).unwrap());
        assert_eq!(msg(r#"{"message":"a"}"#).as_deref(), Some("a"));
        assert_eq!(
            msg(r#"{"object":"error","message":"m","type":"t","code":"c"}"#).as_deref(),
            Some("m")
        );
        assert_eq!(
            msg(r#"{"message":{"detail":[{"msg":"inner"}]}}"#).as_deref(),
            Some("inner")
        );
        assert_eq!(msg(r#"{"message":{"x":1}}"#).as_deref(), Some(r#"{"x":1}"#));
        assert_eq!(msg(r#"{"error":{"message":"e"}}"#).as_deref(), Some("e"));
        assert_eq!(msg(r#"{"error":"flat"}"#).as_deref(), Some("flat"));
        assert_eq!(msg(r#"{"detail":"d"}"#).as_deref(), Some("d"));
        assert_eq!(
            msg(r#"{"detail":[{"msg":"field required","loc":["body"]}]}"#).as_deref(),
            Some("field required")
        );
        assert_eq!(msg(r#"{"message":7}"#), None);
        assert_eq!(msg("[]"), None);
        // `message` wins over `error.message`.
        assert_eq!(
            msg(r#"{"message":"top","error":{"message":"low"}}"#).as_deref(),
            Some("top")
        );
    }

    #[test]
    fn rate_limits() {
        assert_eq!(st(429, &[("retry-after", "3")], "{}"), secs(3));
        assert_eq!(
            st(429, &[], "{}"),
            AiError::RateLimited { retry_after: None }
        );
    }

    #[test]
    fn hostile_retry_after_never_panics() {
        for bad in [
            "-1",
            "1e30",
            "1.5",
            "NaN",
            "inf",
            "",
            "Wed, 21 Oct 2026 07:28:00 GMT",
            "99999999999999999999999999",
        ] {
            assert_eq!(
                st(429, &[("retry-after", bad)], "{}"),
                AiError::RateLimited { retry_after: None },
                "{bad:?}"
            );
        }
    }

    #[test]
    fn huge_retry_after_is_clamped_to_an_hour() {
        assert_eq!(
            st(429, &[("retry-after", "18446744073709551615")], "{}"),
            secs(3600)
        );
        assert_eq!(st(429, &[("retry-after", "3601")], "{}"), secs(3600));
        assert_eq!(st(429, &[("retry-after", "3600")], "{}"), secs(3600));
        assert_eq!(st(429, &[("retry-after", " 7 ")], "{}"), secs(7));
        assert!(
            std::time::Instant::now()
                .checked_add(Duration::from_secs(MAX_RETRY_AFTER_SECS))
                .is_some()
        );
    }

    #[test]
    fn unsupported_on_400_and_422() {
        for phrase in ["not supported", "does not support", "not enabled"] {
            let body = format!(r#"{{"message":"model x {phrase} here"}}"#);
            for code in [400, 422] {
                assert_eq!(
                    st(code, &[], &body),
                    AiError::Unsupported(format!("model x {phrase} here")),
                    "{code} {phrase}"
                );
            }
        }
        assert_eq!(
            st(
                422,
                &[],
                r#"{"detail":[{"msg":"Tools are Not Supported by this model"}]}"#
            ),
            AiError::Unsupported("Tools are Not Supported by this model".into())
        );
        // Other statuses with the same words stay `Provider`.
        assert_eq!(
            st(500, &[], r#"{"message":"not supported"}"#),
            AiError::Provider("HTTP 500: not supported".into())
        );
        assert_eq!(
            st(400, &[], r#"{"message":"bad"}"#),
            AiError::Provider("HTTP 400: bad".into())
        );
    }

    #[test]
    fn body_shapes() {
        assert_eq!(
            st(
                404,
                &[],
                r#"{"object":"error","message":"no such model","type":"invalid_model","code":"1500"}"#
            ),
            AiError::Provider("HTTP 404: no such model".into())
        );
        assert_eq!(
            st(500, &[], r#"{"error":"flat"}"#),
            AiError::Provider("HTTP 500: flat".into())
        );
        assert_eq!(
            st(422, &[], r#"{"detail":[{"msg":"field required"}]}"#),
            AiError::Provider("HTTP 422: field required".into())
        );
        assert_eq!(st(500, &[], ""), AiError::Provider("HTTP 500".into()));
        assert_eq!(st(502, &[], "<html>"), AiError::Provider("HTTP 502".into()));
    }

    #[test]
    fn key_is_redacted_and_text_is_cut() {
        assert_eq!(
            st(500, &[], r#"{"message":"bad mst-secret and mst-secret"}"#),
            AiError::Provider("HTTP 500: bad *** and ***".into())
        );
    }

    #[test]
    fn key_is_scrubbed_before_the_cut() {
        // The key straddles character 200: cut first would leave a prefix of it.
        let text = format!("{}{KEY} tail", "x".repeat(195));
        let body = serde_json::json!({ "message": text }).to_string();
        let AiError::Provider(m) = st(500, &[], &body) else {
            panic!("expected Provider");
        };
        assert!(!m.contains("mst-"), "{m}");
        assert!(m.contains("***"), "{m}");
        let long = format!(r#"{{"message":"{}"}}"#, "é".repeat(500));
        let AiError::Provider(m) = st(500, &[], &long) else {
            panic!("expected Provider");
        };
        assert_eq!(m.chars().count(), "HTTP 500: ".len() + 200);
    }

    #[test]
    fn malformed_json_errors_do_not_quote_the_value() {
        let err = serde_json::from_str::<Vec<u8>>(r#""mst-secret""#).unwrap_err();
        assert!(err.to_string().contains("mst-secret"), "premise");
        let AiError::Protocol(m) = malformed("SSE data", &err) else {
            panic!("expected Protocol");
        };
        assert!(!m.contains("mst-secret"), "{m}");
        assert!(m.starts_with("malformed SSE data: unexpected shape at line 1 column"));
    }

    #[test]
    fn stream_errors() {
        let v: Value = serde_json::from_str(r#"{"message":"overloaded mst-secret"}"#).unwrap();
        assert_eq!(
            stream_error(&v, Some(KEY)),
            AiError::Provider("overloaded ***".into())
        );
        let v: Value = serde_json::from_str(r#"{"error":{}}"#).unwrap();
        assert_eq!(
            stream_error(&v, None),
            AiError::Provider("stream error".into())
        );
    }
}
