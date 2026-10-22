// SPDX-License-Identifier: Apache-2.0
//! Trivial fixture component for the plugin-runtime spike: `add` and a CPU loop `spin`.

// The wit-bindgen macro expands to `unsafe` glue and undocumented items.
#![allow(unsafe_code, missing_docs, clippy::all, clippy::pedantic)]

wit_bindgen::generate!({
    path: "../../wit/spike.wit",
    world: "fixture",
});

struct Fixture;

impl Guest for Fixture {
    fn add(a: u32, b: u32) -> u32 {
        a.wrapping_add(b)
    }

    fn spin(iterations: u64) -> u64 {
        let mut acc: u64 = 0;
        let mut i: u64 = 0;
        while i < iterations {
            acc = acc
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(i | 1);
            i += 1;
        }
        std::hint::black_box(acc)
    }
}

export!(Fixture);
