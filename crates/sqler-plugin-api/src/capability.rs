// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The capability grammar, capability sets and the interface gates (ADR-0056).

use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

use thiserror::Error;

use crate::fs_scope::{FsScope, ScopeError};
use crate::host_pattern::{HostPattern, HostPatternError};
use crate::id::{EngineId, IdError, truncate};

/// Why a capability string was rejected. Echoed input is cut to 64 characters.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum CapabilityError {
    /// The string is empty.
    #[error("capability is empty")]
    Empty,
    /// The string is longer than 300 bytes.
    #[error("capability is longer than 300 bytes")]
    TooLong,
    /// The name is not a known capability.
    #[error("unknown capability `{name}`")]
    Unknown {
        /// The name, truncated.
        name: String,
    },
    /// A capability that needs an argument has none.
    #[error("capability `{}` needs an argument", .kind.name())]
    MissingArgument {
        /// The kind.
        kind: CapabilityKind,
    },
    /// A capability that takes no argument has one.
    #[error("capability `{}` takes no argument", .kind.name())]
    UnexpectedArgument {
        /// The kind.
        kind: CapabilityKind,
    },
    /// The host pattern of `net.http` is invalid.
    #[error("bad host pattern `{pattern}`: {reason}")]
    BadHostPattern {
        /// The pattern, truncated.
        pattern: String,
        /// Why.
        reason: HostPatternError,
    },
    /// The scope of `fs.read` is invalid.
    #[error("bad scope `{scope}`: {reason}")]
    BadScope {
        /// The scope, truncated.
        scope: String,
        /// Why.
        reason: ScopeError,
    },
    /// The engine id of `driver.host` is invalid.
    #[error("bad engine id `{engine}`: {reason}")]
    BadEngineId {
        /// The engine id, truncated.
        engine: String,
        /// Why.
        reason: IdError,
    },
}

/// One granted or requested permission.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum Capability {
    /// `db.read`: catalog access and Read-class statements.
    DbRead,
    /// `db.write`: non-Read statements through the guarded query path.
    DbWrite,
    /// `ui.panel`: panels.
    UiPanel,
    /// `ai.tool`: AI tools.
    AiTool,
    /// `driver.guest`: a driver implemented by the guest.
    DriverGuest,
    /// `net.http:<host-pattern>`.
    NetHttp(HostPattern),
    /// `fs.read:<scope>`.
    FsRead(FsScope),
    /// `driver.host:<engine-id>`: a driver provided by a host engine.
    DriverHost(EngineId),
}

/// The kind of a capability, without its argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum CapabilityKind {
    /// `db.read`.
    DbRead,
    /// `db.write`.
    DbWrite,
    /// `ui.panel`.
    UiPanel,
    /// `ai.tool`.
    AiTool,
    /// `driver.guest`.
    DriverGuest,
    /// `net.http`.
    NetHttp,
    /// `fs.read`.
    FsRead,
    /// `driver.host`.
    DriverHost,
}

impl CapabilityKind {
    /// The name in the grammar, for example `db.read`.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::DbRead => "db.read",
            Self::DbWrite => "db.write",
            Self::UiPanel => "ui.panel",
            Self::AiTool => "ai.tool",
            Self::DriverGuest => "driver.guest",
            Self::NetHttp => "net.http",
            Self::FsRead => "fs.read",
            Self::DriverHost => "driver.host",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "db.read" => Self::DbRead,
            "db.write" => Self::DbWrite,
            "ui.panel" => Self::UiPanel,
            "ai.tool" => Self::AiTool,
            "driver.guest" => Self::DriverGuest,
            "net.http" => Self::NetHttp,
            "fs.read" => Self::FsRead,
            "driver.host" => Self::DriverHost,
            _ => return None,
        })
    }

    fn takes_argument(self) -> bool {
        matches!(self, Self::NetHttp | Self::FsRead | Self::DriverHost)
    }
}

impl fmt::Display for CapabilityKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl Capability {
    /// The kind of this capability.
    #[must_use]
    pub fn kind(&self) -> CapabilityKind {
        match self {
            Self::DbRead => CapabilityKind::DbRead,
            Self::DbWrite => CapabilityKind::DbWrite,
            Self::UiPanel => CapabilityKind::UiPanel,
            Self::AiTool => CapabilityKind::AiTool,
            Self::DriverGuest => CapabilityKind::DriverGuest,
            Self::NetHttp(_) => CapabilityKind::NetHttp,
            Self::FsRead(_) => CapabilityKind::FsRead,
            Self::DriverHost(_) => CapabilityKind::DriverHost,
        }
    }
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NetHttp(p) => write!(f, "net.http:{p}"),
            Self::FsRead(s) => write!(f, "fs.read:{s}"),
            Self::DriverHost(e) => write!(f, "driver.host:{e}"),
            other => f.write_str(other.kind().name()),
        }
    }
}

impl FromStr for Capability {
    type Err = CapabilityError;

    fn from_str(s: &str) -> Result<Self, CapabilityError> {
        if s.is_empty() {
            return Err(CapabilityError::Empty);
        }
        if s.len() > 300 {
            return Err(CapabilityError::TooLong);
        }
        let (name, arg) = match s.split_once(':') {
            Some((n, a)) => (n, Some(a)),
            None => (s, None),
        };
        let kind = CapabilityKind::from_name(name).ok_or_else(|| CapabilityError::Unknown {
            name: truncate(name),
        })?;
        let arg = match (kind.takes_argument(), arg) {
            (true, None | Some("")) => return Err(CapabilityError::MissingArgument { kind }),
            (false, Some(_)) => return Err(CapabilityError::UnexpectedArgument { kind }),
            (_, a) => a.unwrap_or(""),
        };
        Ok(match kind {
            CapabilityKind::DbRead => Self::DbRead,
            CapabilityKind::DbWrite => Self::DbWrite,
            CapabilityKind::UiPanel => Self::UiPanel,
            CapabilityKind::AiTool => Self::AiTool,
            CapabilityKind::DriverGuest => Self::DriverGuest,
            CapabilityKind::NetHttp => {
                Self::NetHttp(
                    arg.parse()
                        .map_err(|reason| CapabilityError::BadHostPattern {
                            pattern: truncate(arg),
                            reason,
                        })?,
                )
            }
            CapabilityKind::FsRead => {
                Self::FsRead(arg.parse().map_err(|reason| CapabilityError::BadScope {
                    scope: truncate(arg),
                    reason,
                })?)
            }
            CapabilityKind::DriverHost => {
                Self::DriverHost(arg.parse().map_err(|reason| CapabilityError::BadEngineId {
                    engine: truncate(arg),
                    reason,
                })?)
            }
        })
    }
}

/// An ordered set of capabilities.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CapabilitySet(BTreeSet<Capability>);

/// What changed between two capability sets.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CapabilityDiff {
    /// In the new set, not in the old one.
    pub added: Vec<Capability>,
    /// In the old set, not in the new one.
    pub removed: Vec<Capability>,
}

impl CapabilityDiff {
    /// Whether nothing changed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty()
    }
}

impl CapabilitySet {
    /// An empty set (default deny).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a capability; returns whether it was new.
    pub fn insert(&mut self, capability: Capability) -> bool {
        self.0.insert(capability)
    }

    /// Whether the exact capability is in the set.
    #[must_use]
    pub fn contains(&self, capability: &Capability) -> bool {
        self.0.contains(capability)
    }

    /// The number of capabilities.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the set is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Iterates in order.
    pub fn iter(&self) -> impl Iterator<Item = &Capability> {
        self.0.iter()
    }

    /// Whether any capability of this kind is in the set.
    #[must_use]
    pub fn allows(&self, kind: CapabilityKind) -> bool {
        self.0.iter().any(|c| c.kind() == kind)
    }

    /// Whether a `net.http` pattern covers `host:port`.
    #[must_use]
    pub fn allows_http(&self, host: &str, port: u16) -> bool {
        self.0
            .iter()
            .any(|c| matches!(c, Capability::NetHttp(p) if p.matches(host, port)))
    }

    /// The `fs.read` scopes.
    pub fn fs_scopes(&self) -> impl Iterator<Item = &FsScope> {
        self.0.iter().filter_map(|c| match c {
            Capability::FsRead(s) => Some(s),
            _ => None,
        })
    }

    /// The `driver.host` engines.
    pub fn driver_engines(&self) -> impl Iterator<Item = &EngineId> {
        self.0.iter().filter_map(|c| match c {
            Capability::DriverHost(e) => Some(e),
            _ => None,
        })
    }

    /// Effective = declared ∩ approved: what both sets contain.
    #[must_use]
    pub fn intersection(&self, other: &Self) -> Self {
        Self(self.0.intersection(&other.0).cloned().collect())
    }

    /// What changed from `previous` to `self`.
    #[must_use]
    pub fn diff(&self, previous: &Self) -> CapabilityDiff {
        CapabilityDiff {
            added: self.0.difference(&previous.0).cloned().collect(),
            removed: previous.0.difference(&self.0).cloned().collect(),
        }
    }
}

impl FromIterator<Capability> for CapabilitySet {
    fn from_iter<I: IntoIterator<Item = Capability>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

/// Every host interface a plugin call can go through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum HostInterface {
    /// `log`.
    Log,
    /// `settings`.
    Settings,
    /// `catalog`.
    Catalog,
    /// `query`.
    Query,
    /// `ui` `set-status` and `notify`.
    Ui,
    /// Panels: `ui` `refresh-panel`, `ui-handler` `render-panel` and `panel-event`.
    UiPanel,
    /// `lifecycle`.
    Lifecycle,
    /// `ui-handler` `run-command` and `status`.
    UiHandler,
    /// `ai-tools`.
    AiTools,
    /// `exporter`.
    Exporter,
    /// `completion`.
    Completion,
    /// `driver`.
    Driver,
    /// WASI outgoing HTTP.
    WasiHttp,
    /// WASI filesystem.
    WasiFilesystem,
}

impl HostInterface {
    /// Every entry.
    pub const ALL: [HostInterface; 14] = [
        Self::Log,
        Self::Settings,
        Self::Catalog,
        Self::Query,
        Self::Ui,
        Self::UiPanel,
        Self::Lifecycle,
        Self::UiHandler,
        Self::AiTools,
        Self::Exporter,
        Self::Completion,
        Self::Driver,
        Self::WasiHttp,
        Self::WasiFilesystem,
    ];

    /// The name of the `sqler:plugin` WIT interface that holds this entry, or `None` for WASI.
    #[must_use]
    pub fn wit_interface(self) -> Option<&'static str> {
        Some(match self {
            Self::Log => "log",
            Self::Settings => "settings",
            Self::Catalog => "catalog",
            Self::Query => "query",
            Self::Ui | Self::UiPanel => "ui",
            Self::Lifecycle => "lifecycle",
            Self::UiHandler => "ui-handler",
            Self::AiTools => "ai-tools",
            Self::Exporter => "exporter",
            Self::Completion => "completion",
            Self::Driver => "driver",
            Self::WasiHttp | Self::WasiFilesystem => return None,
        })
    }
}

/// What a call needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Gate {
    /// No capability (the contribution must be declared in the manifest).
    None,
    /// A capability of this kind (for `net.http` and `fs.read` the argument must also match).
    Requires(CapabilityKind),
    /// The query rule: a Read-class statement needs `db.read`; any other class needs `db.write` and the app's
    /// production gate (ADR-0024), refused when there is no approver (ADR-0036).
    Query,
}

/// The gate of a host interface.
#[must_use]
pub fn gate(interface: HostInterface) -> Gate {
    match interface {
        HostInterface::Log
        | HostInterface::Settings
        | HostInterface::Ui
        | HostInterface::Lifecycle
        | HostInterface::UiHandler
        | HostInterface::Exporter
        | HostInterface::Completion => Gate::None,
        HostInterface::UiPanel => Gate::Requires(CapabilityKind::UiPanel),
        HostInterface::Catalog => Gate::Requires(CapabilityKind::DbRead),
        HostInterface::AiTools => Gate::Requires(CapabilityKind::AiTool),
        HostInterface::Driver => Gate::Requires(CapabilityKind::DriverGuest),
        HostInterface::WasiHttp => Gate::Requires(CapabilityKind::NetHttp),
        HostInterface::WasiFilesystem => Gate::Requires(CapabilityKind::FsRead),
        HostInterface::Query => Gate::Query,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cap(s: &str) -> Capability {
        s.parse().unwrap()
    }

    #[test]
    fn round_trip() {
        for s in [
            "db.read",
            "db.write",
            "ui.panel",
            "ai.tool",
            "driver.guest",
            "net.http:*.example.com:8443",
            "fs.read:data/cache",
            "driver.host:duckdb",
        ] {
            assert_eq!(cap(s).to_string(), s);
        }
    }

    #[test]
    fn errors_reach_every_variant() {
        use CapabilityError as E;
        assert_eq!("".parse::<Capability>(), Err(E::Empty));
        assert_eq!("a".repeat(301).parse::<Capability>(), Err(E::TooLong));
        assert!(matches!(
            "db.exec".parse::<Capability>(),
            Err(E::Unknown { .. })
        ));
        assert!(matches!(
            "DB.read".parse::<Capability>(),
            Err(E::Unknown { .. })
        ));
        assert_eq!(
            "net.http".parse::<Capability>(),
            Err(E::MissingArgument {
                kind: CapabilityKind::NetHttp
            })
        );
        assert_eq!(
            "fs.read:".parse::<Capability>(),
            Err(E::MissingArgument {
                kind: CapabilityKind::FsRead
            })
        );
        assert_eq!(
            "db.read:x".parse::<Capability>(),
            Err(E::UnexpectedArgument {
                kind: CapabilityKind::DbRead
            })
        );
        assert!(matches!(
            "net.http:127.0.0.1".parse::<Capability>(),
            Err(E::BadHostPattern {
                reason: HostPatternError::IpLiteral,
                ..
            })
        ));
        assert!(matches!(
            "fs.read:../x".parse::<Capability>(),
            Err(E::BadScope { .. })
        ));
        assert!(matches!(
            "driver.host:Duck".parse::<Capability>(),
            Err(E::BadEngineId { .. })
        ));
        let long = format!("{}x", "é".repeat(100));
        let Err(E::Unknown { name }) = long.parse::<Capability>() else {
            panic!("expected Unknown");
        };
        assert_eq!(name.chars().count(), 64);
    }

    #[test]
    fn set_queries() {
        let set: CapabilitySet = [
            "db.read",
            "net.http:*.example.com",
            "fs.read:data",
            "driver.host:duckdb",
        ]
        .into_iter()
        .map(cap)
        .collect();
        assert!(set.allows(CapabilityKind::DbRead));
        assert!(!set.allows(CapabilityKind::DbWrite));
        assert!(set.allows_http("api.example.com", 443));
        assert!(!set.allows_http("example.com", 443));
        assert_eq!(set.fs_scopes().count(), 1);
        assert_eq!(set.driver_engines().count(), 1);
        assert_eq!(set.len(), 4);
        assert!(!CapabilitySet::new().allows(CapabilityKind::DbRead));
    }

    #[test]
    fn intersection_and_diff() {
        let a: CapabilitySet = ["db.read", "ui.panel"].into_iter().map(cap).collect();
        let b: CapabilitySet = ["db.read", "ai.tool"].into_iter().map(cap).collect();
        let i = a.intersection(&b);
        assert_eq!(i.iter().collect::<Vec<_>>(), vec![&Capability::DbRead]);
        let d = b.diff(&a);
        assert_eq!(d.added, vec![Capability::AiTool]);
        assert_eq!(d.removed, vec![Capability::UiPanel]);
        assert!(a.diff(&a).is_empty());
    }

    #[test]
    fn gates() {
        assert_eq!(gate(HostInterface::Log), Gate::None);
        assert_eq!(
            gate(HostInterface::UiPanel),
            Gate::Requires(CapabilityKind::UiPanel)
        );
        assert_eq!(
            gate(HostInterface::Catalog),
            Gate::Requires(CapabilityKind::DbRead)
        );
        assert_eq!(gate(HostInterface::Query), Gate::Query);
        assert_eq!(
            gate(HostInterface::WasiHttp),
            Gate::Requires(CapabilityKind::NetHttp)
        );
        assert_eq!(
            gate(HostInterface::WasiFilesystem),
            Gate::Requires(CapabilityKind::FsRead)
        );
        assert_eq!(
            gate(HostInterface::Driver),
            Gate::Requires(CapabilityKind::DriverGuest)
        );
        assert_eq!(
            gate(HostInterface::AiTools),
            Gate::Requires(CapabilityKind::AiTool)
        );
        assert_eq!(HostInterface::ALL.len(), 14);
    }
}
