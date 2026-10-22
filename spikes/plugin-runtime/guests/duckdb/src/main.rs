// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `DuckDB` feasibility guest: `select 1` on `:memory:`, prints `SPIKE guest_duckdb_select1=<v>`.
//!
//! The high-level `duckdb` crate cannot build for wasm32-wasip2 (its unconditional `arrow` ->
//! `comfy-table` -> `crossterm` chain has no WASI backend), so the guest uses the raw C API of
//! `libduckdb-sys` (the bundled `DuckDB` amalgamation). That is also the part this spike has to test.

// The raw C API is `unsafe`; every call site below is a plain FFI call on handles this file owns.
#![allow(unsafe_code)]

mod stubs;

use std::ffi::{CString, c_char};
use std::process::ExitCode;
use std::ptr;

use libduckdb_sys as ffi;

fn run() -> Result<i64, String> {
    let path = CString::new(":memory:").map_err(|e| e.to_string())?;
    let sql = CString::new("select 1").map_err(|e| e.to_string())?;
    let mut db: ffi::duckdb_database = ptr::null_mut();
    let mut con: ffi::duckdb_connection = ptr::null_mut();
    let mut res: ffi::duckdb_result = unsafe { std::mem::zeroed() };
    let mut err: *mut c_char = ptr::null_mut();
    // SAFETY: out-pointers are valid locals; handles are destroyed below in reverse order.
    unsafe {
        if ffi::duckdb_open_ext(path.as_ptr(), &raw mut db, ptr::null_mut(), &raw mut err)
            != ffi::duckdb_state_DuckDBSuccess
        {
            let msg = if err.is_null() {
                "open failed".to_owned()
            } else {
                std::ffi::CStr::from_ptr(err).to_string_lossy().into_owned()
            };
            return Err(msg);
        }
        if ffi::duckdb_connect(db, &raw mut con) != ffi::duckdb_state_DuckDBSuccess {
            ffi::duckdb_close(&raw mut db);
            return Err("connect failed".to_owned());
        }
        let state = ffi::duckdb_query(con, sql.as_ptr(), &raw mut res);
        let out = if state == ffi::duckdb_state_DuckDBSuccess {
            Ok(ffi::duckdb_value_int64(&raw mut res, 0, 0))
        } else {
            let e = ffi::duckdb_result_error(&raw mut res);
            Err(if e.is_null() {
                "query failed".to_owned()
            } else {
                std::ffi::CStr::from_ptr(e).to_string_lossy().into_owned()
            })
        };
        ffi::duckdb_destroy_result(&raw mut res);
        ffi::duckdb_disconnect(&raw mut con);
        ffi::duckdb_close(&raw mut db);
        out
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(v) => {
            println!("SPIKE guest_duckdb_select1={v}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            println!("SPIKE guest_duckdb_select1=fail");
            eprintln!("duckdb error: {e}");
            ExitCode::FAILURE
        }
    }
}
