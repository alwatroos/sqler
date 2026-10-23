// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `catalog` interface through a real component's `HostState`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use crate::error::ErrorCode;
use crate::query::support::SqliteData;
use crate::query::support::wasm::{Fixture, children, columns, command, connections};
use crate::runtime::wit::types::{self as wt, ObjectKind};

fn path(database: &str, schema: &str, table: Option<&str>) -> wt::ObjectPath {
    wt::ObjectPath {
        database: database.to_owned(),
        schema: schema.to_owned(),
        table: table.map(str::to_owned),
    }
}

fn fixture(caps: &[&str], tag: &'static str) -> (Fixture, std::sync::Arc<SqliteData>) {
    Fixture::serving(caps, || SqliteData::new(tag, None, false))
}

fn err<T: std::fmt::Debug>(r: Result<T, wt::PluginError>) -> (ErrorCode, String) {
    let e = r.unwrap_err();
    (e.code.into(), e.message)
}

#[test]
fn the_connections_have_no_target_and_untagged_means_dev() {
    let (f, _src) = fixture(&["db.read"], "cat-conns");
    let list = f.block_on(command(&f, connections())).unwrap().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, "db");
    assert_eq!(list[0].name, "Test DB");
    assert_eq!(list[0].driver, "sqlite");
    assert_eq!(list[0].environment, wt::Environment::Dev);
}

#[test]
fn children_of_a_schema_list_tables_and_the_view() {
    let (f, src) = fixture(&["db.read"], "cat-children");
    let list = f
        .block_on(command(&f, children("db", path("", "main", None))))
        .unwrap()
        .unwrap();
    let mut found: Vec<_> = list.iter().map(|o| (o.kind, o.name.as_str())).collect();
    found.sort_by_key(|(_, n)| *n);
    assert_eq!(
        found,
        [
            (ObjectKind::Table, "orders"),
            (ObjectKind::View, "user_names"),
            (ObjectKind::Table, "users"),
        ]
    );
    assert_eq!(list[0].parent.schema, "main");
    // The second call is answered from the cache.
    let calls = src.introspection_calls();
    assert!(calls > 0, "a cold cache loads through the worker");
    f.block_on(command(&f, children("db", path("", "main", None))))
        .unwrap()
        .unwrap();
    assert_eq!(src.introspection_calls(), calls);
}

#[test]
fn children_of_a_table_list_its_indexes() {
    let (f, _src) = fixture(&["db.read"], "cat-index");
    let list = f
        .block_on(command(&f, children("db", path("", "main", Some("users")))))
        .unwrap()
        .unwrap();
    assert!(
        list.iter()
            .any(|o| o.kind == ObjectKind::Index && o.name == "users_name"),
        "{:?}",
        list.iter().map(|o| (o.kind, &o.name)).collect::<Vec<_>>()
    );
}

#[test]
fn the_columns_of_a_table() {
    let (f, _src) = fixture(&["db.read"], "cat-columns");
    let cols = f
        .block_on(command(&f, columns("db", path("", "main", None), "users")))
        .unwrap()
        .unwrap();
    let got: Vec<_> = cols.iter().map(|c| (c.name.as_str(), c.nullable)).collect();
    assert_eq!(got, [("id", true), ("name", false), ("email", true)]);
    assert!(cols.iter().all(|c| !c.type_name.is_empty()));
    // A view resolves too.
    let cols = f
        .block_on(command(
            &f,
            columns("db", path("", "main", None), "user_names"),
        ))
        .unwrap()
        .unwrap();
    assert_eq!(cols[0].name, "name");
}

#[test]
fn errors_are_coded() {
    let (f, _src) = fixture(&["db.read"], "cat-errors");
    let missing = f
        .block_on(command(&f, columns("db", path("", "main", None), "ghost")))
        .unwrap();
    assert_eq!(err(missing).0, ErrorCode::NotFound);
    let nested = f
        .block_on(command(
            &f,
            columns("db", path("", "main", Some("users")), "id"),
        ))
        .unwrap();
    assert_eq!(err(nested).0, ErrorCode::InvalidArgument);
    let unknown = f
        .block_on(command(&f, children("nope", path("", "", None))))
        .unwrap();
    assert_eq!(err(unknown).0, ErrorCode::NotFound);
}

#[test]
fn no_db_read_is_denied_and_no_service_is_not_available() {
    let (f, _src) = fixture(&[], "cat-denied");
    let r = f.block_on(command(&f, connections())).unwrap();
    assert_eq!(err(r).0, ErrorCode::CapabilityDenied);
    let r = f
        .block_on(command(&f, children("db", path("", "main", None))))
        .unwrap();
    assert_eq!(err(r).0, ErrorCode::CapabilityDenied);
    let f = Fixture::new(&["db.read"], None);
    let r = f.block_on(command(&f, connections())).unwrap();
    assert_eq!(
        err(r),
        (ErrorCode::Failed, "not available in this host".to_owned())
    );
    let f = Fixture::new(&[], None);
    let r = f.block_on(command(&f, connections())).unwrap();
    assert_eq!(err(r).0, ErrorCode::CapabilityDenied);
}

#[test]
fn a_service_without_a_catalog_fails() {
    let (f, _src) = Fixture::serving(&["db.read"], || SqliteData::without_catalog("cat-none"));
    let r = f
        .block_on(command(&f, children("db", path("", "main", None))))
        .unwrap();
    assert_eq!(err(r).0, ErrorCode::Failed);
}
