// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The compiled-in trusted public keys.

/// Base64 text of each trusted 32-byte Ed25519 public key.
///
/// Empty on purpose: R17 adds the production key as
/// `include_str!("../keys/<id>.pub")` entries. Until then
/// [`crate::UpdateClient::check`] returns `NotConfigured` without a request.
pub(crate) const EMBEDDED: &[&str] = &[];
