// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Docker-gated check that a killed server session is classified as a lost connection
//! (see AGENTS.md §3).

#![allow(clippy::unwrap_used)]

mod common;

use sqler_driver_api::sqler_core::Query;
use sqler_driver_api::{DriverError, Session, collect};

use common::{connect, run, start};

async fn rows(session: &mut dyn Session, text: &str) -> Vec<String> {
    let query = Query::new(text);
    let mut stream = session.execute(&query).await.unwrap();
    let rs = collect(&mut *stream).await.unwrap();
    rs.rows().iter().map(|r| r.0[0].to_string()).collect()
}

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
    run(async {
        let Some(server) = start().await else { return };
        let mut a = connect(&server.config()).await;
        let id = rows(&mut *a, "select connection_id() as id")
            .await
            .remove(0);
        let mut b = connect(&server.config()).await;
        rows(&mut *b, "select 1 as k").await;
        let kill_query = Query::new(format!("KILL {id}"));
        let mut kill = b.execute(&kill_query).await.unwrap();
        collect(&mut *kill).await.unwrap();
        drop(kill);
        b.close().await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        let err = failure(&mut *a, "select 2 /* lost */").await;
        assert!(err.is_connection_lost(), "{err:?}");
        assert!(err.lost_before_send(), "{err:?}");
    });
}

#[test]
fn a_missing_table_is_not_a_lost_connection() {
    run(async {
        let Some(server) = start().await else { return };
        let mut s = connect(&server.config()).await;
        let err = failure(&mut *s, "select * from sqler_no_such_table_lost").await;
        assert!(!err.is_connection_lost(), "{err:?}");
        s.close().await.unwrap();
    });
}
