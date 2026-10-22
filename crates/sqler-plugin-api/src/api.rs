// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The ABI version and the compatibility rule (ADR-0055).

use std::fmt;
use std::str::FromStr;

use thiserror::Error;

/// The WIT package name; the package line is `package sqler:plugin@0.1.0;`.
pub const WIT_PACKAGE: &str = "sqler:plugin";

/// The version of the WIT package this crate ships.
pub const API_VERSION: ApiVersion = ApiVersion {
    major: 0,
    minor: 1,
    patch: 0,
};

/// A plugin ABI version.
///
/// While `major` is 0 an additive change bumps `patch` and a breaking one bumps `minor`; from 1.0 on, `minor` and
/// `major`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ApiVersion {
    /// Major version.
    pub major: u32,
    /// Minor version.
    pub minor: u32,
    /// Patch version.
    pub patch: u32,
}

/// The text is not `M.m` or `M.m.p`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("not an API version (expected M.m or M.m.p)")]
pub struct ApiVersionError;

impl ApiVersion {
    /// Builds a version.
    #[must_use]
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// Whether a host speaking `host` can run a plugin that requires `self`.
    ///
    /// For major 0: the same minor and `host.patch >= self.patch`. Otherwise: the same major and
    /// `(minor, patch) <= (host.minor, host.patch)`.
    #[must_use]
    pub fn host_supports(self, host: ApiVersion) -> bool {
        if self.major != host.major {
            return false;
        }
        if self.major == 0 {
            self.minor == host.minor && host.patch >= self.patch
        } else {
            (self.minor, self.patch) <= (host.minor, host.patch)
        }
    }
}

impl fmt::Display for ApiVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl FromStr for ApiVersion {
    type Err = ApiVersionError;

    fn from_str(s: &str) -> Result<Self, ApiVersionError> {
        if s.len() > 32 {
            return Err(ApiVersionError);
        }
        let mut parts = [0u32; 3];
        let mut n = 0;
        for part in s.split('.') {
            if n == 3 || part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
                return Err(ApiVersionError);
            }
            parts[n] = part.parse().map_err(|_| ApiVersionError)?;
            n += 1;
        }
        if n < 2 {
            return Err(ApiVersionError);
        }
        Ok(Self::new(parts[0], parts[1], parts[2]))
    }
}

/// The host cannot run the plugin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum CompatError {
    /// The plugin requires an API version the host does not provide.
    #[error("plugin requires API {required}, host provides {host}")]
    Incompatible {
        /// What the plugin requires.
        required: ApiVersion,
        /// What the host provides.
        host: ApiVersion,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> ApiVersion {
        s.parse().unwrap_or(ApiVersion::new(99, 99, 99))
    }

    #[test]
    fn parse_forms() {
        assert_eq!(v("0.1"), ApiVersion::new(0, 1, 0));
        assert_eq!(v("0.1.3"), ApiVersion::new(0, 1, 3));
        for bad in [
            "",
            "1",
            "1.",
            ".1",
            "1.2.3.4",
            "a.b",
            "+1.2",
            "1.-2",
            "1.2 ",
            "99999999999.1",
        ] {
            assert!(bad.parse::<ApiVersion>().is_err(), "{bad}");
        }
        assert_eq!(v("0.1.0").to_string(), "0.1.0");
        assert_eq!(API_VERSION, v("0.1.0"));
    }

    #[test]
    fn host_supports_matrix() {
        // major 0: same minor, host patch >= required patch
        assert!(v("0.1.0").host_supports(v("0.1.0")));
        assert!(v("0.1.0").host_supports(v("0.1.2")));
        assert!(!v("0.1.2").host_supports(v("0.1.0")));
        assert!(!v("0.1.0").host_supports(v("0.2.0")));
        assert!(!v("0.2.0").host_supports(v("0.1.9")));
        assert!(!v("0.1.0").host_supports(v("1.1.0")));
        // major >= 1
        assert!(v("1.0.0").host_supports(v("1.2.0")));
        assert!(v("1.2.1").host_supports(v("1.2.1")));
        assert!(!v("1.3.0").host_supports(v("1.2.9")));
        assert!(!v("1.2.2").host_supports(v("1.2.1")));
        assert!(!v("2.0.0").host_supports(v("1.9.9")));
        assert!(!v("1.0.0").host_supports(v("2.0.0")));
    }
}
