// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

// The generated host traits are async; these implementations just have nothing to await yet.
#![allow(clippy::unused_async_trait_impl)]

//! The read-only `settings` host interface.

use super::bindings::sqler::plugin::settings as wit;
use super::convert::setting_to_wit;
use super::state::HostState;
use crate::settings::read_setting;

impl wit::Host for HostState {
    async fn get(&mut self, key: String) -> Option<wit::SettingValue> {
        read_setting(&self.plugin.manifest, &*self.settings, &key).and_then(setting_to_wit)
    }
}
