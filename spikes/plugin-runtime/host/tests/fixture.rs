// SPDX-License-Identifier: Apache-2.0
//! Runs the built host against the hello fixture when `SPIKE_FIXTURE_WASM` is set.

use std::process::Command;

fn host() -> Command {
    Command::new(env!("CARGO_BIN_EXE_plugin-spike-host"))
}

#[test]
fn run_fixture_adds() {
    let Ok(wasm) = std::env::var("SPIKE_FIXTURE_WASM") else {
        println!("SKIP: SPIKE_FIXTURE_WASM is not set");
        return;
    };
    let info = host().arg("info").output().unwrap();
    let info = String::from_utf8_lossy(&info.stdout).replace('\r', "");
    if info.contains("SPIKE variant=base") {
        println!("SKIP: the base variant has no engine");
        return;
    }
    let out = host().args(["run-fixture", &wasm]).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).replace('\r', "");
    assert!(out.status.success(), "run-fixture failed: {stdout}");
    assert!(
        stdout.lines().any(|l| l == "SPIKE fixture_add=5"),
        "{stdout}"
    );
}

#[test]
fn usage_error_exits_2() {
    let out = host().arg("bogus").output().unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn info_prints_variant() {
    let out = host().arg("info").output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("SPIKE variant="));
}
