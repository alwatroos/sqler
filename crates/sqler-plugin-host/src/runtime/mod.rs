// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The wasmtime layer: engine, per-plugin stores, calls with deadlines, WASI scope and the host interfaces.
//!
//! Extension protocol for later items: add a `#[cfg(feature = "runtime")] mod <name>;` line in `lib.rs` and one line
//! in [`link::add_imports`], and delete the matching `unbound` file. Host traits go into [`Services`].

mod bindings;
mod call;
mod convert;
mod engine;
mod host;
mod http;
mod instance;
mod limiter;
mod link;
mod log_host;
mod services;
mod settings_host;
mod state;
mod unbound;
mod wasi;

// These instantiate real components, which needs a compiler.
#[cfg(all(test, feature = "cl"))]
mod tests;

// The `ai-tools` call. It lives under `src/ai/` but is compiled as a child of this module, because it reads the
// host's registry and grant source, which are private to `runtime`.
#[path = "../ai/mod.rs"]
mod ai;

pub use ai::{MAX_AI_ARGUMENT_BYTES, MAX_AI_RESULT_BYTES, PluginAiTool};
pub use call::PendingCall;
pub use host::{HostOptions, PluginHost};
pub use services::Services;

// The generated bindings and the error constructor, for the host interfaces that live outside this module (query,
// catalog, ...).
pub(crate) use bindings::exports::sqler::plugin as wit_exports;
pub(crate) use bindings::exports::sqler::plugin::ui_handler;
pub(crate) use bindings::sqler::plugin as wit;
pub(crate) use convert::plugin_error;

// The contract of the extension protocol: later items (P5, P8-P12) name these from their own modules, so they are
// re-exported before anything uses them.
#[allow(unused_imports)]
pub(crate) use call::CallFn;
#[allow(unused_imports)]
pub(crate) use instance::Live;
#[allow(unused_imports)]
pub(crate) use state::HostState;
