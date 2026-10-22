// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

// The generated host traits are async; these implementations just have nothing to await yet.
#![allow(clippy::unused_async_trait_impl)]

//! The `log` host interface and the shared rate limit for every line a plugin writes (this interface, stdout and
//! stderr).

use std::sync::{Arc, Mutex};
use std::time::Instant;

use sqler_plugin_api::PluginId;

use super::bindings::sqler::plugin::log as wit_log;
use super::state::HostState;
use crate::log::{Admit, LogLevel, LogLimiter, emit};

/// One limiter per plugin instance, shared by the `log` interface and the stdio streams.
pub(crate) type SharedLimiter = Arc<Mutex<LogLimiter>>;

/// Writes one line unless the plugin is over its rate; a pass after drops adds one line saying how many were lost.
pub(crate) fn write_limited(
    limiter: &Mutex<LogLimiter>,
    plugin: &PluginId,
    level: LogLevel,
    message: &str,
) {
    let admit = limiter
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .admit(Instant::now());
    match admit {
        Admit::Pass { suppressed } => {
            if suppressed > 0 {
                emit(
                    plugin,
                    LogLevel::Warn,
                    &format!("{suppressed} log lines suppressed (rate limit)"),
                );
            }
            emit(plugin, level, message);
        }
        Admit::Drop => {}
    }
}

fn level(l: wit_log::Level) -> LogLevel {
    match l {
        wit_log::Level::Trace => LogLevel::Trace,
        wit_log::Level::Debug => LogLevel::Debug,
        wit_log::Level::Info => LogLevel::Info,
        wit_log::Level::Warn => LogLevel::Warn,
        wit_log::Level::Error => LogLevel::Error,
    }
}

impl wit_log::Host for HostState {
    async fn write(&mut self, l: wit_log::Level, message: String) {
        write_limited(&self.log, &self.plugin.manifest.id, level(l), &message);
    }
}
