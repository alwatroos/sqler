// SPDX-License-Identifier: Apache-2.0
//! The same fixture as `guests/hello`, bound by `cargo component` (src/bindings.rs is generated).

// The generated glue and the export macro expand to `unsafe` code.
#![allow(unsafe_code)]

#[allow(warnings, missing_docs, clippy::all, clippy::pedantic)]
mod bindings;

use bindings::Guest;

struct Fixture;

impl Guest for Fixture {
    fn add(a: u32, b: u32) -> u32 {
        a.wrapping_add(b)
    }

    fn spin(iterations: u64) -> u64 {
        let mut acc: u64 = 0;
        let mut i: u64 = 0;
        while i < iterations {
            acc = acc.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(i | 1);
            i += 1;
        }
        std::hint::black_box(acc)
    }
}

bindings::export!(Fixture with_types_in bindings);
