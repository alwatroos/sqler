// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The compiled-in trusted public keys.

/// Base64 text of each trusted 32-byte Ed25519 public key.
///
/// One entry per file in `keys/`, named `<key_id>.pub`. With an empty list
/// [`crate::UpdateClient::check`] returns `NotConfigured` without a request.
pub(crate) const EMBEDDED: &[&str] = &[
    // Production key, ADR-0066 note of 2026-10-23.
    include_str!("../keys/efe45a1253fd9347.pub"),
];

/// The file names (without `.pub`) of the entries of [`EMBEDDED`], in the same
/// order. Used by the tests that tie each file name to its key id.
#[cfg(test)]
pub(crate) const EMBEDDED_FILE_IDS: &[&str] = &["efe45a1253fd9347"];
