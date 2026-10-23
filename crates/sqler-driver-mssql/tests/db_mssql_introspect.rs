// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Docker-gated introspection checks (see AGENTS.md §3).

#![allow(clippy::unwrap_used, clippy::too_many_lines)]

mod common;

use sqler_driver_api::sqler_core::{
    DataType, DbObject, Listing, ObjectKind, ObjectPath, ReferentialAction,
};
use sqler_driver_api::{DriverError, Introspect};

use common::{connect, fresh_schema, run, sql, url};

const S: &str = "sqler_t_intro";

async fn names(i: &mut dyn Introspect, parent: &ObjectPath, kind: ObjectKind) -> Vec<String> {
    match i.list(parent, kind).await.unwrap() {
        Listing::Objects(objects) => objects.into_iter().map(|o| o.name).collect(),
        Listing::Unsupported => panic!("{kind:?}: unsupported"),
    }
}

#[test]
fn lists_describes_and_defines() {
    let Some(url) = url() else { return };
    run(async {
        let mut s = connect(&url).await;
        fresh_schema(&mut *s, S).await;
        for stmt in [
            format!(
                "create table {S}.parent (id int not null, name nvarchar(40) null, constraint pk_parent primary key (id))"
            ),
            format!(
                "create table {S}.child (a int not null, b int not null, parent_id int null \
                 constraint df_parent default 1, amount decimal(10, 2) null, \
                 constraint pk_child primary key (a, b), \
                 constraint fk_child_parent foreign key (parent_id) references {S}.parent (id) \
                 on delete cascade on update set null)"
            ),
            format!("create index ix_child_amount on {S}.child (amount)"),
            format!("create view {S}.v as select id, name from {S}.parent"),
            format!("create function {S}.f() returns int as begin return 1 end"),
            format!("create procedure {S}.p as select 1 as one"),
            format!(
                "create trigger {S}.trg on {S}.parent after insert as begin set nocount on end"
            ),
            format!("create sequence {S}.seq as int start with 1"),
            format!("create type {S}.money_t from decimal(10, 2)"),
        ] {
            sql(&mut *s, &stmt).await;
        }

        let i = s.introspection().unwrap();
        for kind in ObjectKind::ALL {
            let supported = !matches!(kind, ObjectKind::MaterializedView | ObjectKind::Extension);
            assert_eq!(i.supports(kind), supported, "{kind:?}");
        }
        assert_eq!(
            i.list(&ObjectPath::root(), ObjectKind::MaterializedView)
                .await
                .unwrap(),
            Listing::Unsupported
        );

        let root = ObjectPath::root();
        assert!(
            names(i, &root, ObjectKind::Database)
                .await
                .contains(&"master".to_owned())
        );
        assert!(
            names(i, &root, ObjectKind::Role)
                .await
                .contains(&"sa".to_owned())
        );

        let db = ObjectPath::root().in_database("master");
        assert!(
            names(i, &db, ObjectKind::Schema)
                .await
                .contains(&S.to_owned())
        );

        let schema = ObjectPath::root().in_database("master").in_schema(S);
        assert_eq!(
            names(i, &schema, ObjectKind::Table).await,
            ["child", "parent"]
        );
        assert_eq!(names(i, &schema, ObjectKind::View).await, ["v"]);
        assert_eq!(names(i, &schema, ObjectKind::Function).await, ["f"]);
        assert_eq!(names(i, &schema, ObjectKind::Procedure).await, ["p"]);
        assert_eq!(names(i, &schema, ObjectKind::Trigger).await, ["trg"]);
        assert_eq!(names(i, &schema, ObjectKind::Sequence).await, ["seq"]);
        assert_eq!(names(i, &schema, ObjectKind::Type).await, ["money_t"]);
        let indexes = names(i, &schema, ObjectKind::Index).await;
        assert!(
            indexes.contains(&"ix_child_amount".to_owned()),
            "{indexes:?}"
        );
        assert!(indexes.contains(&"pk_parent".to_owned()), "{indexes:?}");
        let narrowed = schema.clone().in_table("child");
        let indexes = names(i, &narrowed, ObjectKind::Index).await;
        assert!(!indexes.contains(&"pk_parent".to_owned()), "{indexes:?}");
        let Listing::Objects(triggers) = i.list(&schema, ObjectKind::Trigger).await.unwrap() else {
            panic!("no triggers");
        };
        assert_eq!(triggers[0].parent.table.as_deref(), Some("parent"));

        // Another database cannot be browsed from this session.
        let other = ObjectPath::root().in_database("tempdb");
        let err = i.list(&other, ObjectKind::Schema).await.unwrap_err();
        assert!(matches!(err, DriverError::Unsupported(_)), "{err:?}");

        // table()
        let child = DbObject::new(ObjectKind::Table, schema.clone(), "child");
        let table = i.table(&child).await.unwrap().unwrap();
        let cols: Vec<_> = table.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(cols, ["a", "b", "parent_id", "amount"]);
        assert_eq!(table.column("a").unwrap().data_type, DataType::Integer);
        assert!(!table.column("a").unwrap().nullable);
        assert_eq!(
            table.column("amount").unwrap().native_type.as_deref(),
            Some("decimal(10,2)")
        );
        assert_eq!(table.column("amount").unwrap().data_type, DataType::Decimal);
        assert!(table.column("amount").unwrap().nullable);
        assert!(table.column("parent_id").unwrap().default.is_some());
        let pk = table.primary_key.unwrap();
        assert_eq!(pk.name.as_deref(), Some("pk_child"));
        assert_eq!(pk.columns, ["a", "b"]);
        let fk = &table.foreign_keys[0];
        assert_eq!(fk.name.as_deref(), Some("fk_child_parent"));
        assert_eq!(fk.columns, ["parent_id"]);
        assert_eq!(fk.referenced_schema.as_deref(), Some(S));
        assert_eq!(fk.referenced_table, "parent");
        assert_eq!(fk.referenced_columns, ["id"]);
        assert_eq!(fk.on_delete, ReferentialAction::Cascade);
        assert_eq!(fk.on_update, ReferentialAction::SetNull);

        let parent = DbObject::new(ObjectKind::Table, schema.clone(), "parent");
        let table = i.table(&parent).await.unwrap().unwrap();
        assert_eq!(
            table.column("name").unwrap().native_type.as_deref(),
            Some("nvarchar(40)")
        );

        let view = DbObject::new(ObjectKind::View, schema.clone(), "v");
        assert_eq!(i.table(&view).await.unwrap().unwrap().columns.len(), 2);
        let missing = DbObject::new(ObjectKind::Table, schema.clone(), "nope");
        assert!(i.table(&missing).await.unwrap().is_none());
        let wrong_kind = DbObject::new(ObjectKind::Function, schema.clone(), "f");
        assert!(i.table(&wrong_kind).await.unwrap().is_none());

        // definition()
        let def = |kind, name: &str, table: Option<&str>| {
            let mut path = schema.clone();
            if let Some(table) = table {
                path = path.in_table(table);
            }
            DbObject::new(kind, path, name)
        };
        let text = i
            .definition(&def(ObjectKind::View, "v", None))
            .await
            .unwrap()
            .unwrap();
        assert!(text.to_lowercase().contains("create view"), "{text}");
        let text = i
            .definition(&def(ObjectKind::Function, "f", None))
            .await
            .unwrap()
            .unwrap();
        assert!(text.to_lowercase().contains("return 1"), "{text}");
        let text = i
            .definition(&def(ObjectKind::Procedure, "p", None))
            .await
            .unwrap()
            .unwrap();
        assert!(text.to_lowercase().contains("select 1"), "{text}");
        let text = i
            .definition(&def(ObjectKind::Trigger, "trg", Some("parent")))
            .await
            .unwrap()
            .unwrap();
        assert!(text.to_lowercase().contains("after insert"), "{text}");
        assert!(i.definition(&parent).await.unwrap().is_none());
        assert!(
            i.definition(&def(ObjectKind::View, "nope", None))
                .await
                .unwrap()
                .is_none()
        );
    });
}
