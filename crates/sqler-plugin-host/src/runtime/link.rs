// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The linker: every host interface a plugin may import. One line per interface; later items add theirs to
//! [`add_imports`] and delete the matching `unbound` file.

use wasmtime::component::{HasSelf, Linker};

use super::bindings::sqler::plugin::{catalog, log, query, settings, types, ui};
use super::state::HostState;

/// Links WASI (p2, async), wasi-http, and the `sqler:plugin` imports.
pub(crate) fn add_imports(l: &mut Linker<HostState>) -> wasmtime::Result<()> {
    wasmtime_wasi::p2::add_to_linker_async(l)?;
    wasmtime_wasi_http::p2::add_only_http_to_linker_async(l)?;
    types::add_to_linker::<HostState, HasSelf<HostState>>(l, |s| s)?;
    log::add_to_linker::<HostState, HasSelf<HostState>>(l, |s| s)?;
    settings::add_to_linker::<HostState, HasSelf<HostState>>(l, |s| s)?;
    catalog::add_to_linker::<HostState, HasSelf<HostState>>(l, |s| s)?;
    query::add_to_linker::<HostState, HasSelf<HostState>>(l, |s| s)?;
    ui::add_to_linker::<HostState, HasSelf<HostState>>(l, |s| s)?;
    Ok(())
}
