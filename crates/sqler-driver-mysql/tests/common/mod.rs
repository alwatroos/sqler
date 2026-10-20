// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Helpers shared by the `db_*` test files (ADR-0009). The server is started by
//! `tests/docker/start-mysql.sh`; without `SQLER_TEST_MYSQL_URL` the tests skip.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::future::Future;

use sqler_driver_api::sqler_core::{ConnectionConfig, DriverId};
use sqler_driver_api::{Driver as _, Session};
use sqler_driver_mysql::{DRIVER_ID, MySqlDriver};

/// The root password of the test servers (see `start-mysql.sh`).
pub const PASSWORD: &str = "sqler";

/// The server flavours under test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavour {
    /// `MySQL` 8.4 LTS.
    MySql,
    /// `MariaDB` 11.8 LTS.
    MariaDb,
}

/// The value of `var` if the DB tests for `db` run; prints a skip line and returns `None`
/// otherwise. Panics if `db` is listed in `SQLER_TEST_REQUIRE_DB` but `var` is unset.
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

/// The test server.
pub struct Server {
    pub flavour: Flavour,
    url: String,
    /// The CA that signed the server certificate, if `SQLER_TEST_MYSQL_CA` is set.
    pub ca: Option<String>,
}

impl Server {
    /// The root URL with the default database and no URL parameters.
    pub fn url(&self) -> String {
        self.url.clone()
    }

    /// A URL for `user` with `query` (for example `ssl-mode=required`) appended.
    pub fn url_for(&self, user: &str, password: &str, query: &str) -> String {
        let (scheme, rest) = self.url.split_once("://").unwrap();
        let (_, host) = rest.split_once('@').unwrap();
        let query = if query.is_empty() {
            String::new()
        } else {
            format!("?{query}")
        };
        format!("{scheme}://{user}:{password}@{host}{query}")
    }

    pub fn config(&self) -> ConnectionConfig {
        config(&self.url())
    }

    /// The `host:port` of the server (the URL's authority).
    pub fn addr(&self) -> String {
        let (_, rest) = self.url.split_once("://").unwrap();
        let (_, host) = rest.split_once('@').unwrap();
        let authority = host.split(['/', '?']).next().unwrap();
        if authority.contains(':') {
            authority.to_owned()
        } else {
            format!("{authority}:3306")
        }
    }
}

pub fn config(url: &str) -> ConnectionConfig {
    ConnectionConfig::new("t", DriverId::new(DRIVER_ID).unwrap(), url)
}

pub async fn connect(config: &ConnectionConfig) -> Box<dyn Session> {
    MySqlDriver::new().connect(config).await.unwrap()
}

/// The test server, or `None` (after printing why) when `SQLER_TEST_MYSQL_URL` is unset. The
/// flavour is read from the server's version string.
pub async fn start() -> Option<Server> {
    let url = db_env("SQLER_TEST_MYSQL_URL", "mysql")?;
    let ca = std::env::var("SQLER_TEST_MYSQL_CA")
        .ok()
        .filter(|v| !v.is_empty());
    let mut server = Server {
        flavour: Flavour::MySql,
        url,
        ca,
    };
    let mut session = connect(&server.config()).await;
    let query = sqler_driver_api::sqler_core::Query::new("select version() as v");
    let mut stream = session.execute(&query).await.unwrap();
    let rs = sqler_driver_api::collect(&mut *stream).await.unwrap();
    drop(stream);
    if format!("{:?}", rs.rows()[0].0[0]).contains("MariaDB") {
        server.flavour = Flavour::MariaDb;
    }
    session.close().await.unwrap();
    Some(server)
}

/// Runs `fut` to completion on a fresh Tokio runtime.
pub fn run<F: Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(fut)
}
