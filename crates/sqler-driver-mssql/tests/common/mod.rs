// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Helpers shared by the Docker-gated `db_*` test files (see AGENTS.md §3).

#![allow(dead_code, clippy::unwrap_used)]

use std::future::Future;

use sqler_driver_api::sqler_core::{ConnectionConfig, DriverId, Query, ResultSet, Value};
use sqler_driver_api::{Driver as _, DriverError, Session, collect};
use sqler_driver_mssql::MssqlDriver;

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
    db_env("SQLER_TEST_MSSQL_URL", "mssql")
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
    // The engine never reads `config.driver`; the plugin driver id is `sqlserver`.
    ConnectionConfig::new("t", DriverId::new("sqlserver").unwrap(), target)
}

pub async fn try_connect(config: &ConnectionConfig) -> Result<Box<dyn Session>, DriverError> {
    MssqlDriver::new().connect(config).await
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

/// Drops everything in the schema `name`, then the schema, and creates it again.
pub async fn fresh_schema(session: &mut dyn Session, name: &str) {
    let drop = format!(
        "declare @sch sysname = N'{name}'; declare @s nvarchar(max) = N''; \
         select @s += N'alter table ' + quotename(@sch) + N'.' + quotename(t.name) \
           + N' drop constraint ' + quotename(f.name) + N';' \
           from sys.foreign_keys f join sys.tables t on t.object_id = f.parent_object_id \
           where t.schema_id = schema_id(@sch); \
         select @s += N'drop view ' + quotename(@sch) + N'.' + quotename(name) + N';' \
           from sys.views where schema_id = schema_id(@sch); \
         select @s += N'drop procedure ' + quotename(@sch) + N'.' + quotename(name) + N';' \
           from sys.procedures where schema_id = schema_id(@sch); \
         select @s += N'drop function ' + quotename(@sch) + N'.' + quotename(name) + N';' \
           from sys.objects where type in ('FN', 'IF', 'TF') and schema_id = schema_id(@sch); \
         select @s += N'drop table ' + quotename(@sch) + N'.' + quotename(name) + N';' \
           from sys.tables where schema_id = schema_id(@sch); \
         select @s += N'drop sequence ' + quotename(@sch) + N'.' + quotename(name) + N';' \
           from sys.sequences where schema_id = schema_id(@sch); \
         select @s += N'drop type ' + quotename(@sch) + N'.' + quotename(name) + N';' \
           from sys.types where is_user_defined = 1 and schema_id = schema_id(@sch); \
         exec (@s); \
         if schema_id(@sch) is not null begin set @s = N'drop schema ' + quotename(@sch); exec (@s); end; \
         set @s = N'create schema ' + quotename(@sch); exec (@s);"
    );
    sql(session, &drop).await;
}
