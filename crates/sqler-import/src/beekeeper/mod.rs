// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `Beekeeper Studio` source: the JSON export of saved connections.

mod discover;
mod map;
mod parse;

use std::path::PathBuf;

use crate::error::ImportError;
use crate::model::SourceReport;
use crate::source::{HostEnv, Source};

/// Imports the connection export of `Beekeeper Studio` (a JSON file).
///
/// Maps `PostgreSQL`, `MySQL`/`MariaDB`, `SQLite` and SQL Server connections with host, port,
/// database, user, TLS, colour, folder and SSH tunnel. `DuckDB` entries are skipped as not in this
/// release and every other engine is listed as unsupported. Beekeeper writes passwords into the
/// export in plain text; this importer never reads them (`password`, `sshPassword`,
/// `sshKeyfilePassword` and any URL field are not looked at).
#[derive(Debug, Clone, Copy, Default)]
pub struct Beekeeper;

impl Source for Beekeeper {
    fn id(&self) -> &'static str {
        "beekeeper"
    }

    fn label(&self) -> &'static str {
        "Beekeeper Studio"
    }

    fn default_locations(&self, env: &HostEnv) -> Vec<PathBuf> {
        discover::default_locations(env)
    }

    fn parse(&self, bytes: &[u8]) -> Result<SourceReport, ImportError> {
        parse::parse(bytes)
    }
}
