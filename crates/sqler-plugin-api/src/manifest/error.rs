// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Manifest errors. The first error wins; `field` is a dotted path such as `panels[1].title`.

use thiserror::Error;

use crate::capability::CapabilityError;
use crate::fs_scope::ScopeError;
use crate::id::IdError;

/// Why a manifest was rejected. Echoed input is cut to 64 characters.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum ManifestError {
    /// The input is larger than the limit.
    #[error("manifest is {len} bytes, the limit is {max}")]
    TooLarge {
        /// Actual size.
        len: usize,
        /// Limit.
        max: usize,
    },
    /// The input is not UTF-8.
    #[error("manifest is not valid UTF-8")]
    NotUtf8,
    /// The TOML does not parse.
    #[error("TOML syntax error: {message}")]
    Syntax {
        /// The parser's short message.
        message: String,
        /// The 1-based line, if known.
        line: Option<usize>,
    },
    /// Nested deeper than the limit.
    #[error("manifest is nested too deeply")]
    TooDeep,
    /// A required key is missing.
    #[error("missing field `{field}`")]
    MissingField {
        /// Dotted path.
        field: String,
    },
    /// A key that does not exist.
    #[error("unknown field `{field}`")]
    UnknownField {
        /// Dotted path.
        field: String,
    },
    /// A value of the wrong type.
    #[error("field `{field}` must be {expected}")]
    WrongType {
        /// Dotted path.
        field: String,
        /// What was expected.
        expected: &'static str,
    },
    /// An id outside its grammar.
    #[error("bad id in `{field}`: {reason}")]
    BadId {
        /// Dotted path.
        field: String,
        /// Why.
        reason: IdError,
    },
    /// The plugin version is not semver.
    #[error("bad version `{value}`")]
    BadVersion {
        /// The value, truncated.
        value: String,
    },
    /// The API version is not `M.m` or `M.m.p`.
    #[error("bad API version `{value}`")]
    BadApi {
        /// The value, truncated.
        value: String,
    },
    /// Display text with an empty value, a control character or a bidi override.
    #[error("bad text in `{field}`")]
    BadText {
        /// Dotted path.
        field: String,
    },
    /// A value over its length limit.
    #[error("`{field}` is longer than {max}")]
    TooLong {
        /// Dotted path.
        field: String,
        /// Limit (characters, or bytes for descriptions and schemas).
        max: usize,
    },
    /// A capability that does not parse.
    #[error("capability {index}: {source}")]
    Capability {
        /// Index in the list.
        index: usize,
        /// Why.
        source: CapabilityError,
    },
    /// The same capability twice.
    #[error("duplicate capability `{capability}`")]
    DuplicateCapability {
        /// The capability, truncated.
        capability: String,
    },
    /// A capability that needs another one.
    #[error("capability `{capability}` requires `{requires}`")]
    CapabilityRequires {
        /// The capability.
        capability: String,
        /// What it requires.
        requires: String,
    },
    /// The same id twice in one kind of contribution.
    #[error("duplicate {kind} `{id}`")]
    DuplicateContribution {
        /// The kind, for example `command`.
        kind: &'static str,
        /// The id.
        id: String,
    },
    /// A menu entry names a command that is not declared.
    #[error("`{menu}` names an undeclared command")]
    UnknownCommand {
        /// Dotted path of the menu entry.
        menu: String,
    },
    /// A contribution needs a capability that is not declared.
    #[error("`{contribution}` needs capability `{capability}`")]
    MissingCapability {
        /// The contribution.
        contribution: String,
        /// The capability.
        capability: String,
    },
    /// A contribution needs code but there is no `[entry]`.
    #[error("`{contribution}` needs an [entry]")]
    MissingEntry {
        /// The contribution.
        contribution: String,
    },
    /// The component path is not a safe relative `.wasm` path.
    #[error("bad entry path `{path}`: {reason}")]
    BadEntryPath {
        /// The path, truncated.
        path: String,
        /// Why.
        reason: ScopeError,
    },
    /// A list over its limit.
    #[error("`{field}` has more than {max} entries")]
    TooMany {
        /// Dotted path.
        field: String,
        /// Limit.
        max: usize,
    },
    /// An AI tool input schema that is not a finite JSON object within the size limit.
    #[error("bad input schema for tool `{tool}`")]
    BadSchema {
        /// The tool name, truncated.
        tool: String,
    },
}
