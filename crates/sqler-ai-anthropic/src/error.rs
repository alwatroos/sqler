// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Maps HTTP statuses, API error types and `reqwest` errors to `AiError`.
//!
//! Every string is fixed or comes from the API's own `error.message`, with
//! the API key replaced by `***` and the result cut to 200 characters. None
//! of them holds the URL, a header or the prompt. The key is passed in by the
//! caller; this module never stores it.

use std::time::Duration;

use reqwest::Response;
use sqler_ai_api::AiError;

use crate::wire::ErrorBody;

const MAX_ERROR_CHARS: usize = 200;
/// How much of an error body is read.
const MAX_ERROR_BODY: usize = 64 * 1024;
/// The longest `Retry-After` reported (one hour), so a hostile header cannot
/// make `Instant::now() + retry_after` overflow.
const MAX_RETRY_AFTER_SECS: u64 = 3600;

/// Cuts text to 200 characters.
pub(crate) fn truncate(text: &str) -> String {
    text.chars().take(MAX_ERROR_CHARS).collect()
}

/// Replaces every occurrence of `key` with `***`.
pub(crate) fn scrub(text: &str, key: &str) -> String {
    if key.is_empty() {
        text.to_owned()
    } else {
        text.replace(key, "***")
    }
}

/// Scrubs first, then cuts, so a cut can never leave a piece of the key.
pub(crate) fn clean(text: &str, key: &str) -> String {
    truncate(&scrub(text, key))
}

/// `Retry-After` as whole seconds, clamped to one hour. A date, a float, a
/// negative number or junk gives `None`.
fn retry_after_of(value: Option<&str>) -> Option<Duration> {
    value
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(|n| Duration::from_secs(n.min(MAX_RETRY_AFTER_SECS)))
}

/// Maps a non-success status. `message` is already cleaned.
pub(crate) fn from_status(code: u16, retry_after: Option<&str>, message: &str) -> AiError {
    match code {
        401 | 403 => AiError::Auth(format!("HTTP {code}")),
        429 => AiError::RateLimited {
            retry_after: retry_after_of(retry_after),
        },
        529 => AiError::Overloaded {
            retry_after: retry_after_of(retry_after),
        },
        _ if message.is_empty() => AiError::Provider(format!("HTTP {code}")),
        _ => AiError::Provider(format!("HTTP {code}: {message}")),
    }
}

/// Maps an SSE `error` event, which has no HTTP status. Both strings are
/// already cleaned.
pub(crate) fn from_event_error(kind: &str, message: &str) -> AiError {
    match kind {
        "authentication_error" | "permission_error" => AiError::Auth(kind.to_owned()),
        "rate_limit_error" => AiError::RateLimited { retry_after: None },
        "overloaded_error" => AiError::Overloaded { retry_after: None },
        _ if kind.is_empty() && message.is_empty() => AiError::Provider("error event".to_owned()),
        _ if kind.is_empty() => AiError::Provider(message.to_owned()),
        _ => AiError::Provider(format!("{kind}: {message}")),
    }
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
        "cannot connect to the Anthropic API"
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
pub(crate) async fn from_response(mut resp: Response, key: &str) -> AiError {
    let code = resp.status().as_u16();
    let retry_after = resp
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let body = read_capped(&mut resp, MAX_ERROR_BODY)
        .await
        .unwrap_or_default();
    let message = serde_json::from_slice::<ErrorBody>(&body)
        .ok()
        .and_then(|b| b.error)
        .map(|e| e.message)
        .unwrap_or_default();
    from_status(code, retry_after.as_deref(), &clean(&message, key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostile_retry_after_never_panics() {
        for v in [
            "1e30",
            "-1",
            "NaN",
            "inf",
            "99999999999999999999",
            "",
            "0x10",
        ] {
            assert_eq!(
                from_status(429, Some(v), ""),
                AiError::RateLimited { retry_after: None },
                "{v}"
            );
        }
        // The largest integer that fits is still a valid Duration.
        assert!(matches!(
            from_status(429, Some("18446744073709551615"), ""),
            AiError::RateLimited {
                retry_after: Some(_)
            }
        ));
    }

    #[test]
    fn status_table() {
        assert_eq!(
            from_status(401, None, "x"),
            AiError::Auth("HTTP 401".into())
        );
        assert_eq!(
            from_status(403, None, "x"),
            AiError::Auth("HTTP 403".into())
        );
        assert_eq!(
            from_status(429, Some("7"), ""),
            AiError::RateLimited {
                retry_after: Some(Duration::from_secs(7))
            }
        );
        assert_eq!(
            from_status(429, Some("Wed, 21 Oct 2026 07:28:00 GMT"), ""),
            AiError::RateLimited { retry_after: None }
        );
        assert_eq!(
            from_status(429, None, ""),
            AiError::RateLimited { retry_after: None }
        );
        assert_eq!(
            from_status(529, None, "Overloaded"),
            AiError::Overloaded { retry_after: None }
        );
        assert_eq!(
            from_status(529, Some("99999"), ""),
            AiError::Overloaded {
                retry_after: Some(Duration::from_secs(3600))
            }
        );
        assert_eq!(
            from_status(429, Some("99999"), ""),
            AiError::RateLimited {
                retry_after: Some(Duration::from_secs(3600))
            }
        );
        assert_eq!(
            from_status(400, None, "bad"),
            AiError::Provider("HTTP 400: bad".into())
        );
        assert_eq!(
            from_status(500, None, ""),
            AiError::Provider("HTTP 500".into())
        );
    }

    #[test]
    fn event_error_table() {
        for kind in ["authentication_error", "permission_error"] {
            assert_eq!(from_event_error(kind, "m"), AiError::Auth(kind.into()));
        }
        assert_eq!(
            from_event_error("rate_limit_error", "m"),
            AiError::RateLimited { retry_after: None }
        );
        assert_eq!(
            from_event_error("overloaded_error", "Overloaded"),
            AiError::Overloaded { retry_after: None }
        );
        assert_eq!(
            from_event_error("api_error", "boom"),
            AiError::Provider("api_error: boom".into())
        );
        assert_eq!(
            from_event_error("", ""),
            AiError::Provider("error event".into())
        );
    }

    #[test]
    fn malformed_never_quotes_the_value() {
        let e = serde_json::from_str::<serde_json::Value>("{\"k\": sk-secret-value}").unwrap_err();
        let AiError::Protocol(m) = malformed("SSE event", &e) else {
            panic!("expected Protocol");
        };
        assert!(!m.contains("sk-secret"), "{m}");
        assert!(m.starts_with("malformed SSE event: syntax error at line 1"));
    }

    #[test]
    fn error_text_is_cut_to_200_chars() {
        let long = "\u{e9}".repeat(500);
        assert_eq!(truncate(&long).chars().count(), 200);
        let AiError::Provider(m) = from_status(500, None, &clean(&long, "k")) else {
            panic!("expected Provider");
        };
        assert_eq!(m.chars().count(), "HTTP 500: ".len() + 200);
    }

    #[test]
    fn key_is_scrubbed_before_the_cut() {
        assert_eq!(scrub("a KEY b KEY", "KEY"), "a *** b ***");
        assert_eq!(scrub("abc", ""), "abc");
        // The key straddles the 200-character cut: no piece of it survives.
        let text = format!("{}SECRET-KEY-123", "x".repeat(195));
        let out = clean(&text, "SECRET-KEY-123");
        assert!(!out.contains("SECRET"));
        assert_eq!(out.chars().count(), 198);
        assert!(out.ends_with("***"));
    }
}
