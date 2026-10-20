// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Docker-gated check of the `sqler-sql` float literals against a real server (see AGENTS.md §3):
//! the literal of `-0.0` stores a negative zero on `MySQL`; `MariaDB` 11.8 stores `+0.0` for every
//! literal form (documented limit, ADR-0028). The sign is read with `atan2(0, x)`, which is pi for
//! a negative zero.

#![allow(clippy::unwrap_used)]

mod common;

use sqler_driver_api::sqler_core::{Query, Value};
use sqler_driver_api::{Session, collect};
use sqler_sql::{Dialect, sql_literal};

use common::{Flavour, connect, start};

async fn rows(session: &mut dyn Session, text: &str) -> Vec<Vec<Value>> {
    let q = Query::new(text);
    let mut stream = session.execute(&q).await.unwrap();
    let rs = collect(&mut *stream).await.unwrap();
    rs.rows().iter().map(|r| r.0.clone()).collect()
}

#[test]
fn float_literals_keep_the_sign_of_zero() {
    common::run(async {
        let Some(server) = start().await else { return };
        let mut s = connect(&server.config()).await;
        rows(
            &mut *s,
            "create temporary table zeros (id int, d double, r float)",
        )
        .await;
        for (i, x) in [-0.0_f64, 0.0].into_iter().enumerate() {
            let lit = sql_literal(Dialect::MySql, &Value::Float(x));
            rows(
                &mut *s,
                &format!("insert into zeros values ({i}, {lit}, {lit})"),
            )
            .await;
        }
        let got = rows(
            &mut *s,
            "select atan2(0, d), atan2(0, r) from zeros order by id",
        )
        .await;
        let pi = std::f64::consts::PI;
        let negative = match server.flavour {
            Flavour::MySql => pi,
            Flavour::MariaDb => 0.0,
        };
        let flt = |v: &Value| match v {
            Value::Float(f) => *f,
            other => panic!("{other:?}"),
        };
        assert_eq!(flt(&got[0][0]).to_bits(), negative.to_bits());
        assert_eq!(flt(&got[0][1]).to_bits(), negative.to_bits());
        assert_eq!(flt(&got[1][0]).to_bits(), 0.0_f64.to_bits());
        assert_eq!(flt(&got[1][1]).to_bits(), 0.0_f64.to_bits());
    });
}
