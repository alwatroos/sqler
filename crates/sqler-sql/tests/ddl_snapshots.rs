// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Snapshots of every script for every dialect and kind, plus the unsupported combinations.

use std::fmt::Write as _;

use sqler_core::{
    ColumnInfo, DataType, DbObject, ForeignKey, ObjectKind, ObjectPath, PrimaryKey,
    ReferentialAction, Table,
};
use sqler_sql::{
    DEFAULT_ROW_LIMIT, DdlError, Dialect, Script, create_script, create_table, drop_script,
    insert_template, qualified_name, select_star, supports,
};

const TABLE_NAME: &str = "order\"s";

fn path() -> ObjectPath {
    ObjectPath::root()
        .in_database("my db")
        .in_schema("My Schema")
}

fn object(kind: ObjectKind) -> DbObject {
    match kind {
        ObjectKind::Trigger | ObjectKind::Index => DbObject::new(
            kind,
            path().in_table(TABLE_NAME),
            format!("{}_name", kind.as_str()),
        ),
        ObjectKind::Table => DbObject::new(kind, path(), TABLE_NAME),
        _ => DbObject::new(kind, path(), "Some Name"),
    }
}

fn table() -> Table {
    Table::new(object(ObjectKind::Table))
        .with_columns(vec![
            ColumnInfo::new("id", DataType::Integer)
                .with_native_type("integer")
                .with_nullable(false),
            ColumnInfo::new("select", DataType::Text).with_default("'x'"),
            ColumnInfo::new("Mixed Case", DataType::Timestamp),
            ColumnInfo::new("back`tick", DataType::Other),
        ])
        .with_primary_key(PrimaryKey::new(vec!["id".into()]).with_name("pk_orders"))
        .with_foreign_keys(vec![
            ForeignKey::new(vec!["user id".into()], "users", vec!["id".into()])
                .with_name("fk_user")
                .with_referenced_schema("public")
                .with_on_delete(ReferentialAction::Cascade),
        ])
}

fn definition(dialect: Dialect, kind: ObjectKind) -> Option<&'static str> {
    use Dialect as D;
    match (kind, dialect) {
        (ObjectKind::View | ObjectKind::MaterializedView, D::MySql) => {
            Some("SELECT `id` FROM `order\"s`;")
        }
        (ObjectKind::View | ObjectKind::MaterializedView, _) => {
            Some("SELECT \"id\" FROM \"order\"\"s\";")
        }
        (ObjectKind::Function, D::MySql) => {
            Some("CREATE FUNCTION f() RETURNS int DETERMINISTIC RETURN 1")
        }
        (ObjectKind::Function, _) => Some("CREATE FUNCTION f() RETURNS int AS 'SELECT 1'"),
        (ObjectKind::Procedure, _) => Some("CREATE PROCEDURE p() BEGIN SELECT 1; END"),
        (ObjectKind::Trigger, D::Postgres) => {
            Some("CREATE TRIGGER tr BEFORE INSERT ON t EXECUTE FUNCTION f()")
        }
        (ObjectKind::Trigger, D::MySql) => Some(
            "CREATE TRIGGER tr BEFORE INSERT ON `order\"s` FOR EACH ROW BEGIN SET NEW.id = 1; END",
        ),
        (ObjectKind::Trigger, D::Sqlite) => Some(
            "CREATE TRIGGER tr BEFORE INSERT ON \"order\"\"s\" FOR EACH ROW BEGIN SELECT 1; END",
        ),
        (ObjectKind::Sequence, _) => Some("CREATE SEQUENCE seq START 1;\n"),
        (ObjectKind::Type, _) => Some("CREATE TYPE mood AS ENUM ('sad', 'ok')"),
        (ObjectKind::Index, _) => Some(index_definition(dialect)),
        _ => None,
    }
}

fn index_definition(dialect: Dialect) -> &'static str {
    match dialect {
        Dialect::MySql => "CREATE INDEX `ix` ON `order\"s` (`id`)",
        _ => "CREATE INDEX \"ix\" ON \"order\"\"s\" (\"id\")",
    }
}

fn script_text(dialect: Dialect, kind: ObjectKind, script: Script) -> String {
    let obj = object(kind);
    let result = match script {
        Script::Create if kind == ObjectKind::Table => {
            create_table(dialect, &table(), &[index_definition(dialect).to_owned()])
        }
        Script::Create => create_script(dialect, &obj, definition(dialect, kind)),
        Script::SelectStar => select_star(dialect, &obj, DEFAULT_ROW_LIMIT),
        Script::InsertTemplate => insert_template(dialect, &table()),
        Script::Drop => drop_script(dialect, &obj),
        _ => unreachable!("new script"),
    };
    result.unwrap_or_else(|e| panic!("{dialect:?} {kind:?} {script:?}: {e}"))
}

#[test]
fn snapshot_matrix() {
    let mut files = 0;
    for dialect in Dialect::ALL {
        for kind in ObjectKind::ALL {
            let scripts: Vec<Script> = Script::ALL
                .into_iter()
                .filter(|s| supports(dialect, kind, *s))
                .collect();
            if !scripts.contains(&Script::Create) {
                continue;
            }
            let mut out = String::new();
            for script in scripts {
                writeln!(out, "-- {}", script.as_str()).unwrap();
                out.push_str(&script_text(dialect, kind, script));
                out.push('\n');
            }
            writeln!(
                out,
                "-- copy name: {}",
                qualified_name(dialect, &object(kind)).unwrap()
            )
            .unwrap();
            insta::assert_snapshot!(format!("{}__{}", dialect.as_str(), kind.as_str()), out);
            files += 1;
        }
    }
    assert_eq!(files, 26);
}

#[test]
fn every_unsupported_combination_is_an_error() {
    for dialect in Dialect::ALL {
        for kind in ObjectKind::ALL {
            let obj = object(kind);
            let t = Table::new(obj.clone());
            for script in Script::ALL {
                if supports(dialect, kind, script) {
                    continue;
                }
                let result = match script {
                    Script::Create if kind == ObjectKind::Table => create_table(dialect, &t, &[]),
                    Script::Create => create_script(dialect, &obj, definition(dialect, kind)),
                    Script::SelectStar => select_star(dialect, &obj, 10),
                    Script::InsertTemplate => insert_template(dialect, &t),
                    Script::Drop => drop_script(dialect, &obj),
                    _ => unreachable!("new script"),
                };
                assert_eq!(
                    result,
                    Err(DdlError::Unsupported {
                        dialect,
                        kind,
                        script
                    }),
                    "{dialect:?} {kind:?} {script:?}"
                );
            }
        }
    }
}
