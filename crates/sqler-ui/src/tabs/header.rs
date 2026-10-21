// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! What a tab header shows: title, accent colour, environment badge, kind and preview state.

use super::model::TabKind;

/// A colour as `0xRRGGBB`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u32);

impl Rgb {
    /// Parses `#rrggbb` (hex digits of either case); anything else is `None`.
    #[must_use]
    pub fn parse(hex: &str) -> Option<Self> {
        let digits = hex.strip_prefix('#')?;
        if digits.len() != 6 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        u32::from_str_radix(digits, 16).ok().map(Self)
    }
}

/// The data a tab header renders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabHeader {
    /// The tab title.
    pub title: String,
    /// The connection colour, if it has a valid one.
    pub accent: Option<Rgb>,
    /// The environment badge text: `DEV`, `STAGING` or `PROD`.
    pub badge: Option<&'static str>,
    /// Whether the connection is a production one (rendered distinctly).
    pub is_prod: bool,
    /// What the tab shows.
    pub kind: TabKind,
    /// Whether the tab is a preview (rendered in italics).
    pub preview: bool,
}

/// A header plus the live state a tab strip decorates it with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabDecor {
    /// The header data.
    pub header: TabHeader,
    /// Unsaved text or staged edits.
    pub dirty: bool,
    /// A statement is running.
    pub running: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid() {
        assert_eq!(Rgb::parse("#ff8000"), Some(Rgb(0x00ff_8000)));
        assert_eq!(Rgb::parse("#ABCDEF"), Some(Rgb(0x00ab_cdef)));
        assert_eq!(Rgb::parse("#000000"), Some(Rgb(0)));
    }

    #[test]
    fn rejects_invalid() {
        for bad in [
            "", "#", "ff8000", "#ff80", "#ff80001", "#gg0000", "#+f8000", "#ff 000",
        ] {
            assert_eq!(Rgb::parse(bad), None, "{bad}");
        }
    }
}
