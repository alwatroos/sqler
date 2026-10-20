// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Docker-gated check of the `sqler-sql` float literals against a real server (see AGENTS.md §3):
//! the literal of `-0.0` must store a negative zero, which is compared by text, not by `==`.

#![allow(clippy::unwrap_used)]

mod common;

use sqler_driver_api::sqler_core::Value;
use sqler_sql::{Dialect, sql_literal};

use common::{connect, run, sql, url};

#[test]
fn float_literals_keep_the_sign_of_zero() {
    let Some(url) = url() else { return };
    run(async {
        let mut s = connect(&url).await;
        sql(
            &mut *s,
            "create temp table zeros (id int, d float8, r float4)",
        )
        .await;
        let cases: [(f64, &str); 5] = [
            (-0.0, "-0"),
            (0.0, "0"),
            (f64::NAN, "NaN"),
            (f64::INFINITY, "Infinity"),
            (f64::NEG_INFINITY, "-Infinity"),
        ];
        for (i, (x, _)) in cases.iter().enumerate() {
            let lit = sql_literal(Dialect::Postgres, &Value::Float(*x));
            sql(
                &mut *s,
                &format!("insert into zeros values ({i}, {lit}, {lit})"),
            )
            .await;
        }
        let rs = sql(&mut *s, "select d::text, r::text from zeros order by id").await;
        for (row, (x, want)) in rs.rows().iter().zip(cases) {
            let want = Value::Text(want.to_owned());
            assert_eq!(row.0, vec![want.clone(), want], "{x:?}");
        }
        s.close().await.unwrap();
    });
}

/// `-0.0e0` is a numeric constant on `PostgreSQL`, so it loses the sign; this is why the dialect
/// writes `'-0'::float8`.
#[test]
fn the_exponent_form_would_lose_the_sign() {
    let Some(url) = url() else { return };
    run(async {
        let mut s = connect(&url).await;
        let rs = sql(&mut *s, "select (-0.0e0)::float8::text").await;
        assert_eq!(rs.rows()[0].0, vec![Value::Text("0".to_owned())]);
        s.close().await.unwrap();
    });
}
