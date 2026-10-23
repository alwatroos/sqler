// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `UpdateClient`: one signed GET, no download.

use std::net::IpAddr;
use std::time::Duration;

use reqwest::redirect::{Attempt, Policy};
use reqwest::{Client, Url};

use crate::error::{from_reqwest, read_capped};
use crate::manifest::Download;
use crate::verify::{TrustedKey, embedded_keys, verify_envelope};
use crate::{CONNECT_TIMEOUT, MAX_ENVELOPE_BYTES, MAX_REDIRECTS, REQUEST_TIMEOUT, UpdateError};

/// The result of a successful check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckOutcome {
    /// No newer release. `latest` is what the manifest says (it can be older
    /// than the running version).
    UpToDate {
        /// The version in the manifest.
        latest: semver::Version,
    },
    /// A newer release exists.
    Available(UpdateOffer),
}

/// A newer release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateOffer {
    /// The new version.
    pub version: semver::Version,
    /// `https` URL of the release notes.
    pub notes_url: String,
    /// RFC 3339 date of the release. Informational.
    pub published: String,
    /// The artifact for this platform, `None` when the manifest has none.
    pub download: Option<Download>,
}

/// The Rust target triple of this build when it is one of the five release
/// targets, else `None`.
#[must_use]
pub fn current_target() -> Option<&'static str> {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some("aarch64-apple-darwin")
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Some("x86_64-pc-windows-msvc")
    } else if cfg!(all(target_os = "windows", target_arch = "aarch64")) {
        Some("aarch64-pc-windows-msvc")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("x86_64-unknown-linux-gnu")
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Some("aarch64-unknown-linux-gnu")
    } else {
        None
    }
}

/// Checks a signed manifest for a newer version.
///
/// It sends one `GET` with a `User-Agent` of `SQLer/<version> (update-check)`
/// and no other identifying header, and never downloads an artifact.
#[derive(Debug, Clone)]
pub struct UpdateClient {
    client: Client,
    feed: Url,
    current: semver::Version,
    keys: Vec<TrustedKey>,
    loopback: bool,
}

impl UpdateClient {
    /// Creates a client for `feed_url` using the compiled-in keys.
    ///
    /// # Errors
    ///
    /// [`UpdateError::InvalidFeedUrl`] unless the URL is `https` (or `http` on
    /// a loopback host) with a host and no user name, password, query or
    /// fragment; [`UpdateError::Network`] when the HTTP client cannot be built.
    pub fn new(feed_url: &str, current: semver::Version) -> Result<Self, UpdateError> {
        let feed = Url::parse(feed_url.trim())
            .map_err(|_| UpdateError::InvalidFeedUrl("not a valid URL"))?;
        if !matches!(feed.scheme(), "http" | "https") || feed.host().is_none() {
            return Err(UpdateError::InvalidFeedUrl(
                "must be an http or https URL with a host",
            ));
        }
        if !feed.username().is_empty() || feed.password().is_some() {
            return Err(UpdateError::InvalidFeedUrl(
                "must not contain a user name or password",
            ));
        }
        if feed.query().is_some() || feed.fragment().is_some() {
            return Err(UpdateError::InvalidFeedUrl(
                "must not contain a query or fragment",
            ));
        }
        let loopback = host_is_loopback(&feed);
        if feed.scheme() == "http" && !loopback {
            return Err(UpdateError::InvalidFeedUrl(
                "must use https unless it is a loopback address",
            ));
        }
        let client = build_client(&current, loopback, CONNECT_TIMEOUT, REQUEST_TIMEOUT)?;
        Ok(Self {
            client,
            feed,
            current,
            keys: embedded_keys(),
            loopback,
        })
    }

    /// Replaces the trusted keys (tests and key rotation checks).
    #[must_use]
    pub fn with_trusted_keys(mut self, keys: Vec<TrustedKey>) -> Self {
        self.keys = keys;
        self
    }

    /// Sets the connect and whole-request timeouts.
    ///
    /// # Errors
    ///
    /// [`UpdateError::Network`] when the HTTP client cannot be rebuilt.
    pub fn with_timeouts(
        mut self,
        connect: Duration,
        request: Duration,
    ) -> Result<Self, UpdateError> {
        self.client = build_client(&self.current, self.loopback, connect, request)?;
        Ok(self)
    }

    /// True when at least one trusted key is present.
    #[must_use]
    pub fn is_configured(&self) -> bool {
        !self.keys.is_empty()
    }

    /// Fetches and verifies the manifest and compares versions.
    ///
    /// A version is offered only when it is strictly greater than the running
    /// one and is not a pre-release. An older or equal manifest gives
    /// `UpToDate`, never a downgrade.
    ///
    /// # Errors
    ///
    /// [`UpdateError::NotConfigured`] (before any I/O) without trusted keys;
    /// transport, status, size and verification errors otherwise.
    pub async fn check(&self) -> Result<CheckOutcome, UpdateError> {
        if self.keys.is_empty() {
            return Err(UpdateError::NotConfigured);
        }
        let mut resp = self
            .client
            .get(self.feed.clone())
            .send()
            .await
            .map_err(from_reqwest)?;
        let status = resp.status();
        if !status.is_success() {
            return Err(UpdateError::Status(status.as_u16()));
        }
        if resp
            .content_length()
            .is_some_and(|n| n > MAX_ENVELOPE_BYTES as u64)
        {
            return Err(UpdateError::TooLarge);
        }
        let body = read_capped(&mut resp, MAX_ENVELOPE_BYTES).await?;
        let manifest = verify_envelope(&body, &self.keys)?;
        let latest = semver::Version::parse(&manifest.version)
            .map_err(|_| UpdateError::Malformed("version"))?;
        if latest > self.current && latest.pre.is_empty() {
            let download = current_target().and_then(|t| manifest.platforms.get(t).cloned());
            Ok(CheckOutcome::Available(UpdateOffer {
                version: latest,
                notes_url: manifest.notes_url,
                published: manifest.published,
                download,
            }))
        } else {
            Ok(CheckOutcome::UpToDate { latest })
        }
    }
}

fn build_client(
    current: &semver::Version,
    loopback: bool,
    connect: Duration,
    request: Duration,
) -> Result<Client, UpdateError> {
    let mut builder = Client::builder()
        .connect_timeout(connect)
        .timeout(request)
        .user_agent(format!("SQLer/{current} (update-check)"))
        .redirect(Policy::custom(move |attempt| {
            redirect_decision(loopback, attempt)
        }));
    if loopback {
        // A loopback server must not be reached through an environment proxy.
        builder = builder.no_proxy();
    }
    builder.build().map_err(|_| UpdateError::Network)
}

/// Follows at most `MAX_REDIRECTS` redirects (`previous` includes the feed URL), to `https`, or to `http` on a
/// loopback host when the feed itself is loopback. Never `https` to `http`.
fn redirect_decision(feed_loopback: bool, attempt: Attempt<'_>) -> reqwest::redirect::Action {
    let target = attempt.url();
    let allowed = attempt.previous().len() <= MAX_REDIRECTS
        && target.host().is_some()
        && target.username().is_empty()
        && target.password().is_none()
        && match target.scheme() {
            "https" => true,
            "http" => {
                feed_loopback
                    && host_is_loopback(target)
                    && attempt.previous().iter().all(|u| u.scheme() == "http")
            }
            _ => false,
        };
    if allowed {
        attempt.follow()
    } else {
        attempt.error("redirect refused")
    }
}

fn host_is_loopback(url: &Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    // `host_str` keeps the brackets of an IPv6 literal.
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = bare.parse::<IpAddr>() {
        return ip.is_loopback();
    }
    host.eq_ignore_ascii_case("localhost")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new(url: &str) -> Result<UpdateClient, UpdateError> {
        UpdateClient::new(url, semver::Version::new(1, 0, 0))
    }

    #[test]
    fn feed_url_rules() {
        for bad in [
            "http://example.com/f.json",
            "http://10.0.0.1/f.json",
            "ftp://x/f",
            "not a url",
            "",
            "https://u:p@example.com/f",
            "https://example.com/f?token=1",
            "https://example.com/f#x",
            "file:///x",
        ] {
            assert!(
                matches!(new(bad), Err(UpdateError::InvalidFeedUrl(_))),
                "{bad}"
            );
        }
        for good in [
            crate::DEFAULT_FEED_URL,
            "http://127.0.0.1:1/f.json",
            "http://localhost:1/f.json",
            "http://[::1]:1/f.json",
        ] {
            assert!(new(good).is_ok(), "{good}");
        }
    }

    #[test]
    fn not_configured_without_keys() {
        let c = new(crate::DEFAULT_FEED_URL).unwrap();
        assert!(!c.is_configured());
    }
}
