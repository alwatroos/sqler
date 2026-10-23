// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `pgAdmin 4` source: the `servers.json` export.

mod discover;
mod map;
mod parse;

use std::path::PathBuf;

use crate::error::ImportError;
use crate::model::SourceReport;
use crate::source::{HostEnv, Source};

/// Imports the `servers.json` file that `pgAdmin 4` exports (`Tools > Import/Export Servers`).
///
/// Maps host, port, database, user, TLS mode and files, colour, group and the SSH tunnel. Passwords,
/// `PassFile` and `PasswordExecCommand` are never read or run.
#[derive(Debug, Clone, Copy, Default)]
pub struct PgAdmin;

impl Source for PgAdmin {
    fn id(&self) -> &'static str {
        "pgadmin"
    }

    fn label(&self) -> &'static str {
        "pgAdmin 4"
    }

    fn default_locations(&self, env: &HostEnv) -> Vec<PathBuf> {
        discover::default_locations(env)
    }

    fn parse(&self, bytes: &[u8]) -> Result<SourceReport, ImportError> {
        parse::parse(bytes)
    }
}
