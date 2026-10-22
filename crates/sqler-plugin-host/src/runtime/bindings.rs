// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The generated host bindings for the `sqler:plugin` WIT package (world `full`, async).
//!
//! Only the interfaces are used, never the world: the host links the imports one by one and resolves each export from
//! its `GuestIndices`, because a component that lacks an export would not instantiate through the world.

// No lint allows here: the generated code passes `forbid(unsafe_code)`, `missing_docs` and clippy pedantic as is
// (checked against wasmtime 50.0.0), so the crate keeps the workspace lint table.

wasmtime::component::bindgen!({
    path: "../sqler-plugin-api/wit",
    world: "full",
    imports: { default: async },
    exports: { default: async },
});
