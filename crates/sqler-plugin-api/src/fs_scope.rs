// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Validated `fs.read` scopes (ADR-0056).

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use thiserror::Error;

/// Why a scope (or a relative path) was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum ScopeError {
    /// The scope is empty.
    #[error("scope is empty")]
    Empty,
    /// The root is not `plugin`, `data` or `home`.
    #[error("unknown scope root")]
    UnknownRoot,
    /// `home` without a segment.
    #[error("`home` needs at least one segment")]
    BareHome,
    /// A leading `/`.
    #[error("scope must not be absolute")]
    Absolute,
    /// A `.` or `..` segment.
    #[error("scope must not contain `.` or `..`")]
    Traversal,
    /// An empty segment (`//` or a trailing `/`).
    #[error("scope has an empty segment")]
    EmptySegment,
    /// A segment starting with `.`.
    #[error("scope segments must not start with a dot")]
    HiddenSegment,
    /// A character outside `[A-Za-z0-9._-]` (including `\\`, `:`, `%`, NUL).
    #[error("scope has a character outside [A-Za-z0-9._-]")]
    BadCharacter,
    /// A reserved Windows device name.
    #[error("scope has a reserved device name")]
    Reserved,
    /// A segment over 64 bytes or a scope over 255 bytes.
    #[error("scope is too long")]
    TooLong,
    /// More than 16 segments.
    #[error("scope is nested too deeply")]
    TooDeep,
}

/// The root of a scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum FsRoot {
    /// The plugin's install directory.
    Plugin,
    /// `<config>/plugins/<id>/data`.
    Data,
    /// The user's home directory.
    Home,
}

impl FsRoot {
    fn as_str(self) -> &'static str {
        match self {
            Self::Plugin => "plugin",
            Self::Data => "data",
            Self::Home => "home",
        }
    }
}

/// The directories a scope resolves against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsRoots {
    /// The plugin install directory.
    pub plugin_dir: PathBuf,
    /// The plugin data directory.
    pub data_dir: PathBuf,
    /// The home directory, if known.
    pub home_dir: Option<PathBuf>,
}

/// A validated read-only filesystem scope.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FsScope {
    root: FsRoot,
    segments: Vec<String>,
}

const RESERVED: [&str; 4] = ["con", "nul", "aux", "prn"];

fn is_reserved(segment: &str) -> bool {
    let stem = segment.split('.').next().unwrap_or("").to_ascii_lowercase();
    if RESERVED.contains(&stem.as_str()) {
        return true;
    }
    let b = stem.as_bytes();
    b.len() == 4
        && (stem.starts_with("com") || stem.starts_with("lpt"))
        && (b'1'..=b'9').contains(&b[3])
}

fn check_segment(seg: &str) -> Result<(), ScopeError> {
    if seg.is_empty() {
        return Err(ScopeError::EmptySegment);
    }
    if seg == "." || seg == ".." {
        return Err(ScopeError::Traversal);
    }
    if seg.starts_with('.') {
        return Err(ScopeError::HiddenSegment);
    }
    if seg.len() > 64 {
        return Err(ScopeError::TooLong);
    }
    if !seg
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
        || seg.ends_with('.')
    {
        return Err(ScopeError::BadCharacter);
    }
    if is_reserved(seg) {
        return Err(ScopeError::Reserved);
    }
    Ok(())
}

/// Validates a relative `/`-separated path with the scope segment rules (used for `[entry] component`).
pub(crate) fn check_relative(path: &str) -> Result<Vec<&str>, ScopeError> {
    if path.is_empty() {
        return Err(ScopeError::Empty);
    }
    if path.len() > 255 {
        return Err(ScopeError::TooLong);
    }
    if path.starts_with('/') {
        return Err(ScopeError::Absolute);
    }
    let segments: Vec<&str> = path.split('/').collect();
    if segments.len() > 16 {
        return Err(ScopeError::TooDeep);
    }
    for s in &segments {
        check_segment(s)?;
    }
    Ok(segments)
}

impl FsScope {
    /// The root.
    #[must_use]
    pub fn root(&self) -> FsRoot {
        self.root
    }

    /// The segments below the root.
    #[must_use]
    pub fn segments(&self) -> &[String] {
        &self.segments
    }

    /// Joins the scope onto the matching directory, one segment at a time with `Path::join`.
    ///
    /// Returns `None` for `home` when the home directory is unknown.
    #[must_use]
    pub fn resolve(&self, roots: &FsRoots) -> Option<PathBuf> {
        let mut path = match self.root {
            FsRoot::Plugin => roots.plugin_dir.clone(),
            FsRoot::Data => roots.data_dir.clone(),
            FsRoot::Home => roots.home_dir.clone()?,
        };
        for s in &self.segments {
            path = path.join(s);
        }
        Some(path)
    }
}

impl fmt::Display for FsScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.root.as_str())?;
        for s in &self.segments {
            write!(f, "/{s}")?;
        }
        Ok(())
    }
}

impl FromStr for FsScope {
    type Err = ScopeError;

    fn from_str(s: &str) -> Result<Self, ScopeError> {
        if s.is_empty() {
            return Err(ScopeError::Empty);
        }
        if s.len() > 255 {
            return Err(ScopeError::TooLong);
        }
        if s.contains('\0') {
            return Err(ScopeError::BadCharacter);
        }
        if s.starts_with('/') {
            return Err(ScopeError::Absolute);
        }
        let mut parts = s.split('/');
        let root = match parts.next() {
            Some("plugin") => FsRoot::Plugin,
            Some("data") => FsRoot::Data,
            Some("home") => FsRoot::Home,
            _ => return Err(ScopeError::UnknownRoot),
        };
        let segments: Vec<String> = parts.map(str::to_owned).collect();
        if segments.len() > 16 {
            return Err(ScopeError::TooDeep);
        }
        for seg in &segments {
            check_segment(seg)?;
        }
        if root == FsRoot::Home && segments.is_empty() {
            return Err(ScopeError::BareHome);
        }
        Ok(Self { root, segments })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn accepted() {
        for ok in [
            "plugin",
            "data",
            "data/cache",
            "home/Documents/sql",
            "data/a.b_c-d",
        ] {
            let s: FsScope = ok.parse().unwrap();
            assert_eq!(s.to_string(), ok);
        }
    }

    #[test]
    fn rejected_with_reason() {
        use ScopeError as E;
        let table = [
            ("", E::Empty),
            ("home", E::BareHome),
            ("data/../x", E::Traversal),
            ("data/.", E::Traversal),
            ("/etc", E::Absolute),
            ("data/.ssh", E::HiddenSegment),
            ("C:/x", E::UnknownRoot),
            ("data\\x", E::UnknownRoot),
            ("data/a\\x", E::BadCharacter),
            ("data/a:b", E::BadCharacter),
            ("data/%2e%2e", E::BadCharacter),
            ("data/a\0b", E::BadCharacter),
            ("data/con.txt", E::Reserved),
            ("data/COM1", E::Reserved),
            ("data/lpt9.x.y", E::Reserved),
            ("data/Nul", E::Reserved),
            ("data//x", E::EmptySegment),
            ("data/", E::EmptySegment),
            ("data/x.", E::BadCharacter),
            ("other/x", E::UnknownRoot),
            ("Data/x", E::UnknownRoot),
            ("data/é", E::BadCharacter),
        ];
        for (input, want) in table {
            assert_eq!(input.parse::<FsScope>(), Err(want), "{input:?}");
        }
        assert_eq!(
            format!("data/{}", "a".repeat(65)).parse::<FsScope>(),
            Err(E::TooLong)
        );
        assert_eq!("a".repeat(256).parse::<FsScope>(), Err(E::TooLong));
        let deep = format!("data{}", "/a".repeat(17));
        assert_eq!(deep.parse::<FsScope>(), Err(E::TooDeep));
        assert!(
            format!("data{}", "/a".repeat(16))
                .parse::<FsScope>()
                .is_ok()
        );
        assert!("data/com10".parse::<FsScope>().is_ok());
    }

    #[test]
    fn resolve_uses_path_join() {
        let roots = FsRoots {
            plugin_dir: Path::new("p").join("plug"),
            data_dir: Path::new("d").to_path_buf(),
            home_dir: None,
        };
        let s: FsScope = "data/cache/x".parse().unwrap();
        assert_eq!(
            s.resolve(&roots),
            Some(Path::new("d").join("cache").join("x"))
        );
        let s: FsScope = "plugin".parse().unwrap();
        assert_eq!(s.resolve(&roots), Some(roots.plugin_dir.clone()));
        let s: FsScope = "home/x".parse().unwrap();
        assert_eq!(s.resolve(&roots), None);
        let roots = FsRoots {
            home_dir: Some(Path::new("h").to_path_buf()),
            ..roots
        };
        assert_eq!(s.resolve(&roots), Some(Path::new("h").join("x")));
    }

    #[test]
    fn relative_paths() {
        assert_eq!(check_relative("a/b.wasm").unwrap(), vec!["a", "b.wasm"]);
        assert_eq!(check_relative("../a.wasm"), Err(ScopeError::Traversal));
        assert_eq!(check_relative("/a.wasm"), Err(ScopeError::Absolute));
        assert_eq!(check_relative(""), Err(ScopeError::Empty));
    }
}
