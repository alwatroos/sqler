// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Smoke fixture plugin: the smallest component of the `base` world. `init` writes one log line.

// The wit-bindgen macro expands to `unsafe` FFI glue and undocumented items (AGENTS.md section 4 exception),
// so the generated code and the export live in one module with the lints relaxed.
#[allow(unsafe_code, missing_docs, clippy::all, clippy::pedantic)]
mod bindings {
    wit_bindgen::generate!({
        path: "../../../crates/sqler-plugin-api/wit",
        world: "base",
    });

    use self::exports::sqler::plugin::lifecycle::Guest;
    use self::sqler::plugin::log::{Level, write};

    struct Smoke;

    impl Guest for Smoke {
        fn init() -> Result<(), exports::sqler::plugin::lifecycle::PluginError> {
            write(Level::Info, "smoke: init");
            Ok(())
        }
    }

    export!(Smoke);
}
