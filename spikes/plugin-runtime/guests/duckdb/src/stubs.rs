// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Link-time stand-ins for POSIX calls that wasi-libc does not provide and `DuckDB` still references
//! (memory locking, `madvise`, dynamic loading). Every one fails with `-1`/null, which `DuckDB` treats
//! as "feature unavailable". They are guest code, not patches to `DuckDB`.

#![allow(unsafe_code, clippy::missing_safety_doc)]

use std::ffi::{c_char, c_int, c_void};

#[unsafe(no_mangle)]
pub extern "C" fn mlock(_addr: *const c_void, _len: usize) -> c_int {
    -1
}

#[unsafe(no_mangle)]
pub extern "C" fn munlock(_addr: *const c_void, _len: usize) -> c_int {
    -1
}

#[unsafe(no_mangle)]
pub extern "C" fn madvise(_addr: *mut c_void, _len: usize, _advice: c_int) -> c_int {
    -1
}

#[unsafe(no_mangle)]
pub extern "C" fn dlopen(_file: *const c_char, _mode: c_int) -> *mut c_void {
    std::ptr::null_mut()
}

#[unsafe(no_mangle)]
pub extern "C" fn dlsym(_handle: *mut c_void, _name: *const c_char) -> *mut c_void {
    std::ptr::null_mut()
}

#[unsafe(no_mangle)]
pub extern "C" fn dlerror() -> *mut c_char {
    c"dynamic loading is not available on wasm32-wasip2"
        .as_ptr()
        .cast_mut()
}
