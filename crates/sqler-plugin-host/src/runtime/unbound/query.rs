// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `query`: reads need `db.read`, anything else `db.write` (P5 replaces this file).

use sqler_plugin_api::HostInterface;

use super::gated;
use crate::runtime::bindings::sqler::plugin::query as wit;
use crate::runtime::bindings::sqler::plugin::types::PluginError;
use crate::runtime::state::HostState;

impl wit::Host for HostState {
    async fn run(&mut self, _request: wit::QueryRequest) -> Result<wit::QueryResult, PluginError> {
        Err(gated(&self.effective, HostInterface::Query))
    }
}
