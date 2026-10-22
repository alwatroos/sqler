// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

// The generated host trait is async; the connection listing has nothing to await.
#![allow(clippy::unused_async_trait_impl)]

//! The `catalog` interface: connections and schema objects, read from the `sqler-catalog` cache (ADR-0058).
//!
//! Needs `db.read`. A node that is not cached loads through the connection's read-only catalog worker, so a cold cache
//! does not answer empty. A connection description never carries a target or a secret.

use sqler_catalog::{Catalog, NodeKey};
use sqler_driver_api::sqler_core::{DbObject, Listing, ObjectKind, ObjectPath};
use sqler_plugin_api::CapabilityKind;

use crate::error::ErrorCode;
use crate::query::{
    MAX_CATALOG_ITEMS, NOT_AVAILABLE, PluginData, clip, environment_to_wit, kind_to_wit,
    path_from_wit, path_to_wit, type_name,
};
use crate::runtime::wit::catalog as wit;
use crate::runtime::wit::types::{self as types, PluginError};
use crate::runtime::{HostState, plugin_error};

// These instantiate real components, which needs a compiler.
#[cfg(all(test, feature = "cl"))]
mod tests;

/// The kinds listed under a path, by the deepest field that is set.
fn kinds_under(path: &ObjectPath) -> &'static [ObjectKind] {
    if path.table.is_some() {
        &[ObjectKind::Index, ObjectKind::Trigger]
    } else if path.schema.is_some() {
        &[
            ObjectKind::Table,
            ObjectKind::View,
            ObjectKind::MaterializedView,
            ObjectKind::Function,
            ObjectKind::Procedure,
            ObjectKind::Sequence,
            ObjectKind::Type,
        ]
    } else if path.database.is_some() {
        &[ObjectKind::Schema, ObjectKind::Extension]
    } else {
        &[
            ObjectKind::Database,
            ObjectKind::Schema,
            ObjectKind::Extension,
            ObjectKind::Role,
        ]
    }
}

fn denied() -> PluginError {
    plugin_error(
        ErrorCode::CapabilityDenied,
        "`catalog` is not granted to this plugin",
    )
}

/// Fixed texts only: a driver's introspection error can name the host, the user or the database (see `data_error`).
fn catalog_error(e: &sqler_catalog::CatalogError) -> PluginError {
    use sqler_catalog::CatalogError;
    let text = match e {
        CatalogError::Cancelled => "the operation was cancelled",
        CatalogError::Closed => "the catalog is closed",
        CatalogError::NoIntrospection => "the connection does not support introspection",
        _ => "the catalog could not be read",
    };
    plugin_error(ErrorCode::Failed, text)
}

impl HostState {
    /// The gate shared by the three calls: `db.read`, then the data the application provided.
    fn catalog_data(&self) -> Result<std::sync::Arc<PluginData>, PluginError> {
        if !self.effective.allows(CapabilityKind::DbRead) {
            return Err(denied());
        }
        self.services
            .get::<PluginData>()
            .ok_or_else(|| plugin_error(ErrorCode::Failed, NOT_AVAILABLE))
    }
}

/// The catalog of `connection`, after checking that plugins may see the connection.
fn catalog_of(data: &PluginData, connection: &str) -> Result<Catalog, PluginError> {
    if !data
        .source()
        .connections()
        .iter()
        .any(|c| c.id == connection)
    {
        return Err(plugin_error(
            ErrorCode::NotFound,
            format!("unknown connection `{}`", clip(connection)),
        ));
    }
    data.source().catalog(connection).ok_or_else(|| {
        plugin_error(
            ErrorCode::Failed,
            format!("connection `{}` has no catalog", clip(connection)),
        )
    })
}

impl wit::Host for HostState {
    async fn connections(&mut self) -> Result<Vec<types::ConnectionInfo>, PluginError> {
        let data = self.catalog_data()?;
        Ok(data
            .source()
            .connections()
            .into_iter()
            .map(|c| types::ConnectionInfo {
                id: c.id,
                name: c.name,
                driver: c.driver.to_string(),
                environment: environment_to_wit(c.environment),
            })
            .collect())
    }

    async fn children(
        &mut self,
        connection: String,
        parent: types::ObjectPath,
    ) -> Result<Vec<types::DbObject>, PluginError> {
        let data = self.catalog_data()?;
        let catalog = catalog_of(&data, &connection)?;
        let path = path_from_wit(parent);
        let mut out = Vec::new();
        for kind in kinds_under(&path) {
            let key = NodeKey::new(path.clone(), *kind);
            let listing = match catalog.cached(&key) {
                Some(hit) => hit,
                None => catalog.children(key).await.map_err(|e| catalog_error(&e))?,
            };
            let Listing::Objects(objects) = &*listing else {
                continue;
            };
            for o in objects {
                let Some(kind) = kind_to_wit(o.kind) else {
                    continue;
                };
                if out.len() >= MAX_CATALOG_ITEMS {
                    return Err(plugin_error(
                        ErrorCode::LimitExceeded,
                        format!("more than {MAX_CATALOG_ITEMS} objects"),
                    ));
                }
                out.push(types::DbObject {
                    kind,
                    parent: path_to_wit(o.parent.clone()),
                    name: o.name.clone(),
                });
            }
        }
        Ok(out)
    }

    async fn columns(
        &mut self,
        connection: String,
        table: types::ObjectPath,
        name: String,
    ) -> Result<Vec<types::ColumnInfo>, PluginError> {
        let data = self.catalog_data()?;
        let catalog = catalog_of(&data, &connection)?;
        let path = path_from_wit(table);
        if path.table.is_some() {
            return Err(plugin_error(
                ErrorCode::InvalidArgument,
                "`table.table` must be unset; the table is `name`",
            ));
        }
        for kind in [
            ObjectKind::Table,
            ObjectKind::View,
            ObjectKind::MaterializedView,
        ] {
            let object = DbObject::new(kind, path.clone(), name.clone());
            let found = catalog.table(object).await.map_err(|e| catalog_error(&e))?;
            if let Some(found) = found {
                if found.columns.len() > MAX_CATALOG_ITEMS {
                    return Err(plugin_error(
                        ErrorCode::LimitExceeded,
                        format!("more than {MAX_CATALOG_ITEMS} columns"),
                    ));
                }
                return Ok(found
                    .columns
                    .iter()
                    .map(|c| types::ColumnInfo {
                        name: c.name.clone(),
                        type_name: type_name(c.native_type.as_deref(), c.data_type),
                        nullable: c.nullable,
                    })
                    .collect());
            }
        }
        Err(plugin_error(
            ErrorCode::NotFound,
            format!("no table or view `{}`", clip(&name)),
        ))
    }
}

#[cfg(test)]
mod error_tests {
    use super::*;
    use sqler_catalog::CatalogError;

    #[test]
    fn a_driver_error_text_never_reaches_the_guest() {
        let secret = "connect to db.prod.example:5432 as admin password=hunter2 failed";
        let e = catalog_error(&CatalogError::Driver(secret.to_owned()));
        assert_eq!(ErrorCode::from(e.code), ErrorCode::Failed);
        assert_eq!(e.message, "the catalog could not be read");
        assert_eq!(
            catalog_error(&CatalogError::NoIntrospection).message,
            "the connection does not support introspection"
        );
    }
}
