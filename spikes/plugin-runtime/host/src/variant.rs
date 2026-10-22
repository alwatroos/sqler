// SPDX-License-Identifier: Apache-2.0
//! Which engine variant this binary was built as. With several engine features enabled
//! (for example `clippy --all-features`) the first matching `cfg` wins; no `compile_error!`.

/// The variant id (`base`, `cl`, `cl-default`, `pulley`, `pulley-rt`, `winch`).
pub const NAME: &str = name();

const fn name() -> &'static str {
    if cfg!(feature = "cl-default") {
        "cl-default"
    } else if cfg!(feature = "pulley") {
        "pulley"
    } else if cfg!(feature = "winch") {
        "winch"
    } else if cfg!(feature = "pulley-rt") {
        "pulley-rt"
    } else if cfg!(feature = "cl") {
        "cl"
    } else {
        "base"
    }
}

/// The wasmtime version resolved from `Cargo.lock` (`none` for `base`).
pub const WASMTIME_VERSION: &str = env!("SPIKE_WASMTIME_VERSION");

/// The wasmtime cargo features this variant enables (the host crate sets `default-features = false`).
pub const fn wasmtime_features() -> &'static str {
    match name().as_bytes() {
        b"cl-default" => "default(all wasmtime defaults)+wasmtime-wasi/p2",
        b"pulley" => "runtime,component-model,async,std,cranelift,pulley+wasmtime-wasi/p2",
        b"winch" => "runtime,component-model,async,std,winch+wasmtime-wasi/p2",
        b"pulley-rt" => "runtime,component-model,async,std,pulley+wasmtime-wasi/p2",
        b"cl" => "runtime,component-model,async,std,cranelift+wasmtime-wasi/p2",
        _ => "none",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_is_one_of_the_variants() {
        assert!(["base", "cl", "cl-default", "pulley", "pulley-rt", "winch"].contains(&NAME));
    }

    #[test]
    fn base_has_no_features() {
        if NAME == "base" {
            assert_eq!(wasmtime_features(), "none");
            assert_eq!(WASMTIME_VERSION, "none");
        }
    }
}
