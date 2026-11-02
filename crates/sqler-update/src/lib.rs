// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Update client for `SQLer` (ADR-0066, ADR-0078).
//!
//! [`UpdateClient::check`] fetches one signed envelope from the GitHub
//! Releases `latest/download` URL, verifies its Ed25519 signature against the
//! compiled-in public keys, and reports whether a newer release exists. It
//! sends only a generic `User-Agent` (`SQLer/<version> (update-check)`), and
//! makes no request at all while no trusted key is compiled in
//! ([`UpdateError::NotConfigured`]).
//!
//! [`UpdateClient::download`] fetches the signed artifact for this platform and
//! checks its size and sha256. [`install()`] re-hashes a copy and swaps the
//! program (or the whole `SQLer.app`) in place with a rollback.
//!
//! The crate is headless and depends on no other `sqler-*` crate. Whether and
//! when to check is the caller's decision (opt-in, see [`CheckSchedule`]).
//!
//! # Tokio
//!
//! [`UpdateClient::check`] must be polled inside a Tokio runtime with I/O and
//! time enabled. Dropping the future cancels the request.

mod client;
mod download;
mod error;
mod install;
mod keys;
#[cfg(target_os = "macos")]
mod macos;
mod manifest;
mod policy;
mod verify;

use std::time::Duration;

pub use client::{CheckOutcome, UpdateClient, UpdateOffer, current_target};
pub use download::{ArtifactKind, VerifiedArtifact, artifact_file_name, verify_file};
pub use error::UpdateError;
pub use install::{
    InstallHooks, InstallOutcome, InstallPlan, ManualReason, clean_leftovers, default_package_dirs,
    install, plan_install,
};
#[cfg(target_os = "macos")]
pub use macos::{codesign_check, hdiutil_extract_app};
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
/// Default time allowed for the whole check request.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// The largest artifact (archive or disk image) accepted, in bytes.
pub const MAX_ARTIFACT_BYTES: u64 = 256 << 20;
/// The largest program extracted from an archive, in bytes.
pub const MAX_BINARY_BYTES: u64 = 512 << 20;
/// How long a download may receive no data before it fails.
pub const DOWNLOAD_STALL_TIMEOUT: Duration = Duration::from_secs(60);
/// The folder under the config root that holds downloads.
pub const UPDATES_DIR_NAME: &str = "updates";
/// The staging folder beside the program that is replaced.
pub const STAGE_DIR_NAME: &str = ".sqler-update";
