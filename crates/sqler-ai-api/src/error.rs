// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

use std::time::Duration;

use thiserror::Error;

/// Errors an AI provider can return.
///
/// The strings in these variants never carry an API key, request headers or
/// prompt text. Providers must scrub them before building an error.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum AiError {
    /// The credentials were missing or rejected.
    #[error("authentication failed: {0}")]
    Auth(String),
    /// The provider throttled the request.
    #[error("rate limited")]
    RateLimited {
        /// How long the provider asked the caller to wait, if it said.
        retry_after: Option<Duration>,
    },
    /// The provider is overloaded (HTTP 529 or an overload error in a stream).
    /// Retried like a rate limit; carries no server text.
    #[error("provider overloaded")]
    Overloaded {
        /// How long the provider asked the caller to wait, if it said.
        retry_after: Option<Duration>,
    },
    /// The network failed or the provider was unreachable.
    #[error("network error: {0}")]
    Network(String),
    /// The provider reported an error.
    #[error("provider error: {0}")]
    Provider(String),
    /// The request was cancelled.
    #[error("request cancelled")]
    Cancelled,
    /// The provider does not support the request.
    #[error("unsupported: {0}")]
    Unsupported(String),
    /// The request is malformed.
    #[error("invalid request: {0}")]
    Invalid(String),
    /// A provider broke the stream protocol.
    #[error("provider protocol violation: {0}")]
    Protocol(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages() {
        let cases = [
            (AiError::Auth("x".into()), "authentication failed: x"),
            (AiError::RateLimited { retry_after: None }, "rate limited"),
            (
                AiError::Overloaded { retry_after: None },
                "provider overloaded",
            ),
            (AiError::Network("x".into()), "network error: x"),
            (AiError::Provider("x".into()), "provider error: x"),
            (AiError::Cancelled, "request cancelled"),
            (AiError::Unsupported("x".into()), "unsupported: x"),
            (AiError::Invalid("x".into()), "invalid request: x"),
            (
                AiError::Protocol("x".into()),
                "provider protocol violation: x",
            ),
        ];
        for (e, s) in cases {
            assert_eq!(e.to_string(), s);
        }
    }
}
