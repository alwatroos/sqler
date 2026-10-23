// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Validation of a path taken from a foreign file. The path is never opened or looked up.

use std::path::PathBuf;

const MAX_PATH_BYTES: usize = 4_096;

const RESERVED: [&str; 4] = ["CON", "PRN", "AUX", "NUL"];

fn is_reserved(component: &str) -> bool {
    let trimmed = component.trim_end_matches(['.', ' ']);
    let stem = trimmed
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end_matches(' ');
    let upper = stem.to_ascii_uppercase();
    if RESERVED.contains(&upper.as_str()) {
        return true;
    }
    for prefix in ["COM", "LPT"] {
        if let Some(rest) = upper.strip_prefix(prefix)
            && rest.len() == 1
            && matches!(rest.as_bytes()[0], b'1'..=b'9')
        {
            return true;
        }
    }
    false
}

/// Validates a path from a foreign file and returns it.
///
/// Refused: an empty path, control characters (NUL included), more than 4,096 bytes, the verbatim
/// and device prefixes `\\?\`, `\\.\`, `//?/` and `//./`, a component named like a Windows
/// reserved device (`CON`, `PRN`, `AUX`, `NUL`, `COM1`-`COM9`, `LPT1`-`LPT9`, with or without an
/// extension, on every OS), and a path that is not absolute on this OS (`~`, `$HOME`,
/// `<STORAGE_DIR>` and relative paths are not expanded).
///
/// # Errors
/// A fixed reason that never echoes the input.
pub fn foreign_path(raw: &str) -> Result<PathBuf, String> {
    let path = raw.trim();
    if path.is_empty() {
        return Err("path is empty".to_owned());
    }
    if path.len() > MAX_PATH_BYTES {
        return Err("path is too long".to_owned());
    }
    if path.chars().any(char::is_control) {
        return Err("path contains control characters".to_owned());
    }
    if ["\\\\?\\", "\\\\.\\", "//?/", "//./"]
        .iter()
        .any(|p| path.starts_with(p))
    {
        return Err("verbatim and device paths are not accepted".to_owned());
    }
    if path.split(['/', '\\']).any(is_reserved) {
        return Err("path uses a reserved device name".to_owned());
    }
    let buf = PathBuf::from(path);
    if !buf.is_absolute() {
        return Err("path is not absolute".to_owned());
    }
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    const ABS: &str = "/home/alice/certs/ca.pem";
    #[cfg(windows)]
    const ABS: &str = "C:\\Users\\alice\\certs\\ca.pem";

    #[test]
    fn accepts_absolute() {
        assert_eq!(foreign_path(ABS).unwrap(), PathBuf::from(ABS));
        assert_eq!(
            foreign_path(&format!("  {ABS} \n")).unwrap(),
            PathBuf::from(ABS)
        );
    }

    #[test]
    fn refuses_each_class() {
        let long = format!("/{}", "a".repeat(5_000));
        let cases: Vec<&str> = vec![
            "",
            "   ",
            "/tmp/a\0b",
            "/tmp/a\nb",
            &long,
            "\\\\?\\C:\\x\\ca.pem",
            "\\\\.\\COM1",
            "//?/C:/x",
            "//./pipe/x",
            "/tmp/con",
            "/tmp/CON.txt",
            "/tmp/nul.tar.gz",
            "/tmp/aux ",
            "/tmp/prn...",
            "/tmp/com1",
            "/tmp/LPT9.log",
            "C:\\x\\aux\\file",
            "~/certs/ca.pem",
            "$HOME/certs/ca.pem",
            "<STORAGE_DIR>/ca.pem",
            "certs/ca.pem",
            "./ca.pem",
        ];
        for case in cases {
            assert!(foreign_path(case).is_err(), "{case:?}");
        }
    }

    #[test]
    fn near_misses_are_fine() {
        for name in [
            "/tmp/com0",
            "/tmp/com10",
            "/tmp/console",
            "/tmp/lpt",
            "/tmp/auxiliary.pem",
        ] {
            #[cfg(unix)]
            assert!(foreign_path(name).is_ok(), "{name}");
            #[cfg(windows)]
            let _ = name;
        }
    }

    #[test]
    fn error_never_echoes_input() {
        let e = foreign_path("secret-token-xyz/relative").unwrap_err();
        assert!(!e.contains("secret-token-xyz"));
    }
}
