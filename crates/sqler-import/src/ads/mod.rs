// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The Azure Data Studio source: the user `settings.json`.

mod discover;
mod jsonc;
mod map;
mod parse;

use std::path::PathBuf;

use crate::error::ImportError;
use crate::model::SourceReport;
use crate::source::{HostEnv, Source};

/// Imports the saved connections of Azure Data Studio from its user `settings.json`
/// (`datasource.connections` and `datasource.connectionGroups`; comments and trailing commas are
/// accepted).
///
/// SQL Server (`MSSQL`) entries map to the `sqlserver` driver and `PostgreSQL` (`PGSQL`) entries to
/// `postgres`; SQL login only. Windows and Azure AD authentication, other providers and named
/// instances are skipped with a reason. The group tree and the group colour are mapped. Stored
/// passwords are never read.
#[derive(Debug, Clone, Copy, Default)]
pub struct AzureDataStudio;

impl Source for AzureDataStudio {
    fn id(&self) -> &'static str {
        "azure-data-studio"
    }

    fn label(&self) -> &'static str {
        "Azure Data Studio"
    }

    fn default_locations(&self, env: &HostEnv) -> Vec<PathBuf> {
        discover::default_locations(env)
    }

    fn parse(&self, bytes: &[u8]) -> Result<SourceReport, ImportError> {
        parse::parse(bytes)
    }
}
