// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Helpers shared by the Docker-gated `db_*` test files. Copy this file to other drivers.

#![allow(dead_code, clippy::unwrap_used)]

use std::future::Future;

use sqler_driver_api::sqler_core::{ConnectionConfig, DriverId, Query, ResultSet, Value};
use sqler_driver_api::{Driver as _, DriverError, Session, collect};
use sqler_driver_postgres::{DRIVER_ID, PostgresDriver};

/// The URL if the DB tests for `db` run; prints a skip line and returns `None` otherwise.
/// Panics if `db` is listed in `SQLER_TEST_REQUIRE_DB` but `var` is unset.
pub fn db_env(var: &str, db: &str) -> Option<String> {
    if let Ok(value) = std::env::var(var)
        && !value.is_empty()
    {
        return Some(value);
    }
    let required = std::env::var("SQLER_TEST_REQUIRE_DB")
        .is_ok_and(|list| list.split(',').any(|id| id.trim() == db));
    assert!(
        !required,
        "{var} is not set, but {db} is listed in SQLER_TEST_REQUIRE_DB"
    );
    println!("skipped: {var} is not set (see AGENTS.md §3)");
    None
}

/// The URL of the test server, if the DB tests run.
pub fn url() -> Option<String> {
    db_env("SQLER_TEST_POSTGRES_URL", "postgres")
}

/// Runs a future on a fresh current-thread Tokio runtime.
pub fn run<F: Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(fut)
}

pub fn config(target: &str) -> ConnectionConfig {
    ConnectionConfig::new("t", DriverId::new(DRIVER_ID).unwrap(), target)
}

pub async fn try_connect(config: &ConnectionConfig) -> Result<Box<dyn Session>, DriverError> {
    PostgresDriver::new().connect(config).await
}

pub async fn connect(url: &str) -> Box<dyn Session> {
    try_connect(&config(url)).await.unwrap()
}

pub async fn query(session: &mut dyn Session, q: &Query) -> Result<ResultSet, DriverError> {
    let mut stream = session.execute(q).await?;
    collect(&mut *stream).await
}

pub async fn sql(session: &mut dyn Session, text: &str) -> ResultSet {
    query(session, &Query::new(text)).await.unwrap()
}

pub async fn sql_params(session: &mut dyn Session, text: &str, params: Vec<Value>) -> ResultSet {
    query(session, &Query::new(text).with_params(params))
        .await
        .unwrap()
}

/// Drops and recreates the schema `name` and makes it the session's `search_path`.
pub async fn fresh_schema(session: &mut dyn Session, name: &str) {
    sql(session, &format!("drop schema if exists {name} cascade")).await;
    sql(session, &format!("create schema {name}")).await;
    sql(session, &format!("set search_path to {name}")).await;
}

/// Forwards `127.0.0.1:<returned port>` to `127.0.0.1:port`; stands in for an SSH tunnel.
#[cfg(unix)]
pub async fn forwarder(port: u16) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut inbound, _)) = listener.accept().await {
            tokio::spawn(async move {
                if let Ok(mut outbound) = tokio::net::TcpStream::connect(("127.0.0.1", port)).await
                {
                    let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
                }
            });
        }
    });
    local
}
