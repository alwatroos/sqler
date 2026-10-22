// SPDX-License-Identifier: Apache-2.0
//! Exports the resolved wasmtime version (from `Cargo.lock`) as `SPIKE_WASMTIME_VERSION`.

use std::fs;

fn main() {
    println!("cargo:rerun-if-changed=../Cargo.lock");
    let lock = fs::read_to_string("../Cargo.lock").unwrap_or_default();
    let mut version = String::from("none");
    let has_engine = std::env::var_os("CARGO_FEATURE_ENGINE").is_some();
    let mut lines = lock.lines();
    while let Some(line) = lines.next() {
        if has_engine && line == "name = \"wasmtime\"" {
            if let Some(v) = lines.next().and_then(|l| l.strip_prefix("version = \"")) {
                v.trim_end_matches('"').clone_into(&mut version);
            }
            break;
        }
    }
    println!("cargo:rustc-env=SPIKE_WASMTIME_VERSION={version}");
}
