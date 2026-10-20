// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Headless command-line interface: run a query, export results and start the `MCP` server. Proves the core is UI-independent.

#![forbid(unsafe_code)]

fn main() {
    println!("sqler-cli {}", env!("CARGO_PKG_VERSION"));
}
