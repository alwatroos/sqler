// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `sqler-sql` float literals against a real `SQLite`: `-0.0e0` is a negative zero as an
//! expression (and in a column without REAL affinity), but a REAL column stores `+0.0`. The sign
//! is read back as `f64` bits through the driver (the bundled build has no math functions).

#![allow(clippy::unwrap_used)]

use sqler_driver_api::sqler_core::{ConnectionConfig, DriverId, Query, Value};
use sqler_driver_api::{Driver as _, collect};
use sqler_driver_sqlite::{DRIVER_ID, SqliteDriver};
use sqler_sql::{Dialect, sql_literal};

#[test]
fn negative_zero_literal_and_storage() {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(async {
            let config = ConnectionConfig::new("t", DriverId::new(DRIVER_ID).unwrap(), ":memory:");
            let mut s = SqliteDriver::new().connect(&config).await.unwrap();
            let lit = sql_literal(Dialect::Sqlite, &Value::Float(-0.0));
            for text in [
                "create table real_col (x real)".to_owned(),
                "create table any_col (x)".to_owned(),
                format!("insert into real_col values ({lit})"),
                format!("insert into any_col values ({lit})"),
            ] {
                let q = Query::new(text);
                let mut stream = s.execute(&q).await.unwrap();
                collect(&mut *stream).await.unwrap();
            }
            let sql = format!("select (select x from any_col), (select x from real_col), {lit}");
            let q = Query::new(sql);
            let mut stream = s.execute(&q).await.unwrap();
            let rs = collect(&mut *stream).await.unwrap();
            let bits: Vec<u64> = rs.rows()[0]
                .0
                .iter()
                .map(|v| match v {
                    Value::Float(x) => x.to_bits(),
                    other => panic!("{other:?}"),
                })
                .collect();
            // As an expression and in an untyped column the sign survives; a REAL column drops it.
            assert_eq!(
                bits,
                [
                    (-0.0_f64).to_bits(),
                    0.0_f64.to_bits(),
                    (-0.0_f64).to_bits()
                ]
            );
        });
}
