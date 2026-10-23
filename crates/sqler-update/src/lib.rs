// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Check-only update client for `SQLer` (ADR-0066).
//!
//! [`UpdateClient::check`] fetches one signed envelope from the GitHub
//! Releases `latest/download` URL, verifies its Ed25519 signature against the
//! compiled-in public keys, and reports whether a newer release exists. It
//! never downloads or installs a binary, sends only a generic `User-Agent`
//! (`SQLer/<version> (update-check)`), and makes no request at all while no
//! trusted key is compiled in ([`UpdateError::NotConfigured`]).
//!
//! The crate is headless and depends on no other `sqler-*` crate. Whether and
//! when to check is the caller's decision (opt-in, see [`CheckSchedule`]).
//!
//! # Tokio
//!
//! [`UpdateClient::check`] must be polled inside a Tokio runtime with I/O and
//! time enabled. Dropping the future cancels the request.

mod client;
mod error;
mod keys;
mod manifest;
mod policy;
mod verify;

use std::time::Duration;

pub use client::{CheckOutcome, UpdateClient, UpdateOffer, current_target};
pub use error::UpdateError;
pub use manifest::{Download, Envelope, Manifest, parse_manifest};
pub use policy::CheckSchedule;
pub use verify::{TrustedKey, embedded_keys, key_id, signing_message, verify_envelope};

/// The feed: the manifest asset of the latest GitHub release.
pub const DEFAULT_FEED_URL: &str =
    "https://github.com/alwatroos/sqler/releases/latest/download/sqler-update.json";
/// The file name of the envelope asset on a release.
pub const MANIFEST_FILE_NAME: &str = "sqler-update.json";
/// The largest envelope accepted, in bytes.
pub const MAX_ENVELOPE_BYTES: usize = 64 * 1024;
/// The most redirects followed.
pub const MAX_REDIRECTS: usize = 3;
/// Domain separation prefix of the signed message.
pub const SIGNING_CONTEXT: &[u8] = b"sqler-update-manifest-v1\0";
/// Default time allowed to connect.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Default time allowed for the whole request.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
