// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `pulley-rt` builds and links, but a build without Cranelift cannot load a component (ADR-0054: precompiled
//! loading needs `unsafe` and is decided with the engine choice).

#![cfg(all(feature = "pulley-rt", not(feature = "cl")))]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::Arc;

use sqler_plugin_api::PluginId;
use sqler_plugin_host::{CallError, LoadError};

use common::{Grants, Scratch, block_on, discover_all, host, runtime, test_limits, write_plugin};

#[test]
fn loading_returns_no_compiler() {
    let scratch = Scratch::new("nocompiler");
    let d = discover_all(&[write_plugin(scratch.path(), "alpha", "ok", &[])]);
    let rt = runtime();
    let host = host(
        &rt,
        Arc::new(Grants::default()),
        &test_limits(std::time::Duration::from_secs(5)),
    );
    assert_eq!(host.load(d).len(), 0);
    let id: PluginId = "alpha".parse().unwrap();
    let e = block_on(&rt, host.init(&id)).unwrap_err();
    assert_eq!(e, CallError::Load(LoadError::NoCompiler));
    host.shutdown();
}
