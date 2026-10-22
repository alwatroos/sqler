// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `catalog`: needs `db.read` (P5 replaces this file).

use sqler_plugin_api::HostInterface;

use super::gated;
use crate::runtime::bindings::sqler::plugin::catalog as wit;
use crate::runtime::bindings::sqler::plugin::types::{
    ColumnInfo, ConnectionInfo, DbObject, ObjectPath, PluginError,
};
use crate::runtime::state::HostState;

impl wit::Host for HostState {
    async fn connections(&mut self) -> Result<Vec<ConnectionInfo>, PluginError> {
        Err(gated(&self.effective, HostInterface::Catalog))
    }

    async fn children(
        &mut self,
        _connection: String,
        _parent: ObjectPath,
    ) -> Result<Vec<DbObject>, PluginError> {
        Err(gated(&self.effective, HostInterface::Catalog))
    }

    async fn columns(
        &mut self,
        _connection: String,
        _table: ObjectPath,
        _name: String,
    ) -> Result<Vec<ColumnInfo>, PluginError> {
        Err(gated(&self.effective, HostInterface::Catalog))
    }
}
