// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Build script: embeds the application icon into the Windows executable.

fn main() -> Result<(), String> {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=resources/sqler.rc");
    println!("cargo:rerun-if-changed=../../assets/icon/sqler.ico");
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "windows" {
        embed_resource::compile(
            "resources/sqler.rc",
            embed_resource::ParamsIncludeDirs(["../../assets/icon"]),
        )
        .manifest_optional()
        .map_err(|e| format!("cannot embed the application icon: {e}"))?;
    }
    Ok(())
}
