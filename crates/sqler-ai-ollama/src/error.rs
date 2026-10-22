// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Maps HTTP statuses and `reqwest` errors to `AiError`.
//!
//! Every string is fixed or comes from Ollama's own `error` field, cut to 200
//! characters. None of them holds the URL, a header or the prompt.

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

/// Maps a non-success status. `error` is the text of Ollama's `error` field.
pub(crate) fn from_status(code: u16, retry_after: Option<&str>, error: &str) -> AiError {
    let text = truncate(error);
    match code {
        401 | 403 => AiError::Auth(format!("HTTP {code}")),
        429 => AiError::RateLimited {
            retry_after: retry_after
                .and_then(|v| v.trim().parse::<u64>().ok())
                .map(|n| Duration::from_secs(n.min(MAX_RETRY_AFTER_SECS))),
        },
        400 if error.contains("does not support") => AiError::Unsupported(text),
        _ if text.is_empty() => AiError::Provider(format!("HTTP {code}")),
        _ => AiError::Provider(format!("HTTP {code}: {text}")),
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
        "cannot connect to the Ollama server"
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
pub(crate) async fn from_response(mut resp: Response) -> AiError {
    let code = resp.status().as_u16();
    let retry_after = resp
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let body = read_capped(&mut resp, MAX_ERROR_BODY)
        .await
        .unwrap_or_default();
    let text = serde_json::from_slice::<ErrorBody>(&body)
        .map(|b| b.error)
        .unwrap_or_default();
    from_status(code, retry_after.as_deref(), &text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_never_quotes_the_value() {
        let e = serde_json::from_str::<serde_json::Value>("{\"k\": sk-secret-value}").unwrap_err();
        let AiError::Protocol(m) = malformed("data", &e) else {
            panic!("expected Protocol");
        };
        assert!(!m.contains("sk-secret"), "{m}");
        assert!(m.starts_with("malformed data: syntax error at line 1"));
    }

    #[test]
    fn status_table() {
        assert!(matches!(from_status(401, None, "x"), AiError::Auth(_)));
        assert!(matches!(from_status(403, None, "x"), AiError::Auth(_)));
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
            from_status(429, Some("99999"), ""),
            AiError::RateLimited {
                retry_after: Some(Duration::from_secs(3600))
            }
        );
        assert_eq!(
            from_status(429, None, ""),
            AiError::RateLimited { retry_after: None }
        );
        assert!(matches!(
            from_status(400, None, "m does not support tools"),
            AiError::Unsupported(_)
        ));
        assert_eq!(
            from_status(400, None, "bad"),
            AiError::Provider("HTTP 400: bad".into())
        );
        assert_eq!(
            from_status(404, None, "model 'x' not found"),
            AiError::Provider("HTTP 404: model 'x' not found".into())
        );
        assert_eq!(
            from_status(500, None, ""),
            AiError::Provider("HTTP 500".into())
        );
    }

    #[test]
    fn error_text_is_cut_to_200_chars() {
        let long = "é".repeat(500);
        let AiError::Provider(m) = from_status(500, None, &long) else {
            panic!("expected Provider");
        };
        assert_eq!(m.chars().count(), "HTTP 500: ".len() + 200);
        assert_eq!(truncate(&long).chars().count(), 200);
    }
}
