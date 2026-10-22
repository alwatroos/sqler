// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! API-compatible stand-in that finds no platform certificates (the guest trusts an explicit CA).

use rustls_pki_types::CertificateDer;

/// Same shape as the real crate's result; `errors` is always empty.
pub struct CertificateResult {
    pub certs: Vec<CertificateDer<'static>>,
    pub errors: Vec<std::io::Error>,
}

#[must_use]
pub fn load_native_certs() -> CertificateResult {
    CertificateResult { certs: Vec::new(), errors: Vec::new() }
}
