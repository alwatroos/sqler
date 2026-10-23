// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The error type of the update check.
//!
//! Every text is fixed. None holds a URL, a header, a path or server data.

use reqwest::Response;

/// Why an update check failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum UpdateError {
    /// No trusted public key is compiled in, so nothing can be verified. No
    /// request was made.
    #[error("update checks are not configured in this build")]
    NotConfigured,
    /// The feed URL is not acceptable.
    #[error("invalid update feed URL: {0}")]
    InvalidFeedUrl(&'static str),
    /// The feed host cannot be reached.
    #[error("cannot reach the update server")]
    Offline,
    /// The server did not answer in time.
    #[error("the update server timed out")]
    Timeout,
    /// Any other transport failure, including a response cut short.
    #[error("the update request failed")]
    Network,
    /// Too many redirects, or a redirect the policy refuses.
    #[error("the update server redirected the request to a refused location")]
    Redirect,
    /// The server answered with a status other than success.
    #[error("the update server answered HTTP {0}")]
    Status(u16),
    /// The response is larger than the cap.
    #[error("the update manifest is too large")]
    TooLarge,
    /// The envelope or the manifest is not valid.
    #[error("malformed update manifest: {0}")]
    Malformed(&'static str),
    /// The envelope `format` is not supported.
    #[error("unsupported update manifest format")]
    UnsupportedFormat,
    /// The envelope names a key that is not trusted.
    #[error("the update manifest is signed with an unknown key")]
    UnknownKey,
    /// The signature does not match the payload.
    #[error("the update manifest signature is invalid")]
    BadSignature,
}

/// Maps a transport error. The URL is removed first.
pub(crate) fn from_reqwest(e: reqwest::Error) -> UpdateError {
    let e = e.without_url();
    if e.is_timeout() {
        UpdateError::Timeout
    } else if e.is_connect() {
        UpdateError::Offline
    } else if e.is_redirect() {
        UpdateError::Redirect
    } else {
        UpdateError::Network
    }
}

/// Reads at most `cap` bytes of a body.
pub(crate) async fn read_capped(resp: &mut Response, cap: usize) -> Result<Vec<u8>, UpdateError> {
    let mut out = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(from_reqwest)? {
        out.extend_from_slice(&chunk);
        if out.len() > cap {
            return Err(UpdateError::TooLarge);
        }
    }
    Ok(out)
}
