// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Docker-gated check that a killed server session is classified as a lost connection
//! (see AGENTS.md §3).

#![allow(clippy::unwrap_used)]

mod common;

use sqler_driver_api::sqler_core::Query;
use sqler_driver_api::{DriverError, Session, collect};

use common::{connect, run, sql, url};

/// Runs `text` on `session` and returns the error from `execute` or from the stream.
async fn failure(session: &mut dyn Session, text: &str) -> DriverError {
    let query = Query::new(text);
    match session.execute(&query).await {
        Err(e) => e,
        Ok(mut stream) => collect(&mut *stream).await.unwrap_err(),
    }
}

#[test]
fn a_killed_session_is_a_lost_connection() {
    let Some(url) = url() else { return };
    run(async {
        let mut a = connect(&url).await;
        let spid = sql(&mut *a, "select @@SPID as p").await.rows()[0].0[0].to_string();
        let mut b = connect(&url).await;
        sql(&mut *b, &format!("KILL {spid}")).await;
        b.close().await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        let err = failure(&mut *a, "select 2 /* lost */").await;
        assert!(err.is_connection_lost(), "{err:?}");
    });
}

#[test]
fn a_missing_table_is_not_a_lost_connection() {
    let Some(url) = url() else { return };
    run(async {
        let mut s = connect(&url).await;
        let err = failure(&mut *s, "select * from sqler_no_such_table_lost").await;
        assert!(!err.is_connection_lost(), "{err:?}");
        s.close().await.unwrap();
    });
}
