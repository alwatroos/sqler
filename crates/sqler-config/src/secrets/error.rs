// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The typed error of the secrets API.

use thiserror::Error;

use super::SecretRef;

/// Why a secret operation failed.
///
/// Every message names the reference and never a secret value.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SecretError {
    /// A reference string or connection id broke the grammar.
    #[error("invalid secret reference {input:?}: {reason}")]
    InvalidRef {
        /// The rejected input (a connection id or a whole reference, never a secret value).
        input: String,
        /// Which rule it broke.
        reason: &'static str,
    },
    /// The keychain cannot be used: missing, locked, access denied or unsupported platform.
    #[error("keychain unavailable for {reference}: {reason}")]
    Unavailable {
        /// The reference being accessed.
        reference: SecretRef,
        /// The backend's explanation.
        reason: String,
    },
    /// The keychain backend failed.
    #[error("keychain error for {reference}: {reason}")]
    Backend {
        /// The reference being accessed.
        reference: SecretRef,
        /// The backend's explanation.
        reason: String,
    },
    /// The keychain refused the entry (a length limit or an invalid parameter).
    #[error("keychain rejected {reference}: {reason}")]
    Rejected {
        /// The reference being accessed.
        reference: SecretRef,
        /// The backend's explanation.
        reason: String,
    },
    /// The stored value is not valid UTF-8.
    #[error("stored secret for {reference} is not valid UTF-8")]
    NotUtf8 {
        /// The reference being read.
        reference: SecretRef,
    },
    /// More than one keychain entry matches the reference.
    #[error("more than one keychain entry matches {reference}")]
    Ambiguous {
        /// The reference being accessed.
        reference: SecretRef,
    },
}
