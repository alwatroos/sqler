// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Hover: columns (type, nullability, foreign key), relations, CTEs and subqueries, and the
//! places where nothing is shown.

use sqler_completion::{
    Hover, HoverRequest, HoverTarget, MemorySchema, RelationInfo, RelationKind, hover,
};
use sqler_core::{ColumnInfo, DataType, ForeignKey, ObjectKind};
use sqler_sql::Dialect;

fn home(d: Dialect) -> &'static str {
    match d {
        Dialect::MySql => "shop",
        Dialect::Sqlite => "main",
        _ => "public",
    }
}

fn schema(d: Dialect) -> MemorySchema {
    let h = home(d);
    let mut s = MemorySchema::new([h]);
    s.add_table(
        h,
        "users",
        ObjectKind::Table,
        Some(vec![
            ColumnInfo::new("id", DataType::Integer)
                .with_native_type("integer")
                .with_nullable(false),
            ColumnInfo::new("name", DataType::Text).with_native_type("text"),
            ColumnInfo::new("Mixed", DataType::Text),
        ]),
    );
    s.add_table(
        h,
        "orders",
        ObjectKind::Table,
        Some(vec![
            ColumnInfo::new("id", DataType::Integer).with_nullable(false),
            ColumnInfo::new("user_id", DataType::Integer)
                .with_native_type("integer")
                .with_nullable(false),
            ColumnInfo::new("total", DataType::Decimal).with_native_type("numeric(10,2)"),
        ]),
    );
    let mut fk = ForeignKey::new(vec!["user_id".to_owned()], "users", vec!["id".to_owned()]);
    fk.referenced_schema = Some(h.to_owned());
    s.add_foreign_key(h, "orders", fk);
    s.add_table(h, "pending", ObjectKind::Table, None);
    s.add_table(
        h,
        "active_users",
        ObjectKind::View,
        Some(vec![ColumnInfo::new("id", DataType::Integer)]),
    );
    s.add_table(
        "reporting",
        "totals",
        ObjectKind::MaterializedView,
        Some(vec![ColumnInfo::new("sum", DataType::Decimal)]),
    );
    s
}

/// Hovers at the `|` marker; returns the hovered text and the result.
fn at(sql: &str, d: Dialect) -> (String, Option<Hover>) {
    let Some(off) = sql.find('|') else {
        panic!("no marker in {sql}");
    };
    let text = sql.replacen('|', "", 1);
    let h = hover(&HoverRequest::new(&text, off, d), &schema(d));
    let shown = h
        .as_ref()
        .map_or(String::new(), |h| text[h.range.clone()].to_owned());
    (shown, h)
}

fn pg(sql: &str) -> (String, Option<HoverTarget>) {
    let (s, h) = at(sql, Dialect::Postgres);
    (s, h.map(|h| h.target))
}

fn rel(schema: Option<&str>, name: &str, kind: RelationKind) -> RelationInfo {
    RelationInfo::new(schema, name, kind)
}

fn column(t: Option<HoverTarget>) -> (RelationInfo, ColumnInfo, Option<ForeignKey>) {
    match t {
        Some(HoverTarget::Column {
            relation,
            info,
            references,
        }) => (relation, info, references),
        other => panic!("not a column: {other:?}"),
    }
}

#[test]
fn column_through_alias() {
    let (shown, t) = pg("SELECT u.na|me FROM users u");
    assert_eq!(shown, "name");
    let (relation, info, fk) = column(t);
    assert_eq!(relation, rel(Some("public"), "users", RelationKind::Table));
    assert_eq!(info.name, "name");
    assert_eq!(info.data_type, DataType::Text);
    assert_eq!(info.native_type.as_deref(), Some("text"));
    assert!(info.nullable);
    assert_eq!(fk, None);
}

#[test]
fn column_nullability_and_type() {
    let (_, t) = pg("SELECT u.i|d FROM users u");
    let (_, info, _) = column(t);
    assert!(!info.nullable);
    assert_eq!(info.native_type.as_deref(), Some("integer"));
}

#[test]
fn column_with_foreign_key() {
    let (shown, t) = pg("SELECT o.user_i|d FROM orders o");
    assert_eq!(shown, "user_id");
    let (relation, info, fk) = column(t);
    assert_eq!(relation.name, "orders");
    assert_eq!(info.name, "user_id");
    let fk = fk.expect("foreign key");
    assert_eq!(fk.referenced_table, "users");
    assert_eq!(fk.referenced_columns, ["id"]);
    assert_eq!(fk.referenced_schema.as_deref(), Some("public"));
    // A column without one.
    let (_, t) = pg("SELECT o.tot|al FROM orders o");
    assert_eq!(column(t).2, None);
}

#[test]
fn column_through_bare_table_name_and_unqualified() {
    let (_, t) = pg("SELECT users.na|me FROM users");
    assert_eq!(column(t).1.name, "name");
    let (_, t) = pg("SELECT na|me FROM users");
    assert_eq!(column(t).1.name, "name");
    let (_, t) = pg("SELECT 1 FROM users u JOIN orders o ON o.user_id = u.id WHERE tot|al > 1");
    let (relation, info, _) = column(t);
    assert_eq!(
        (relation.name.as_str(), info.name.as_str()),
        ("orders", "total")
    );
    let (_, t) = pg("SELECT 1 FROM users WHERE users.id IN (SELECT user_i|d FROM orders)");
    assert_eq!(column(t).0.name, "orders");
}

#[test]
fn schema_qualified_column() {
    let (shown, t) = pg("SELECT public.users.na|me FROM public.users");
    assert_eq!(shown, "name");
    assert_eq!(column(t).1.name, "name");
    // Not in the FROM clause: resolved through the schema.
    let (_, t) = pg("SELECT public.users.na|me");
    assert_eq!(column(t).1.name, "name");
    let (shown, t) = pg("SELECT public.us|ers.name FROM public.users");
    assert_eq!(shown, "users");
    assert_eq!(
        t,
        Some(HoverTarget::Relation(rel(
            Some("public"),
            "users",
            RelationKind::Table
        )))
    );
    assert_eq!(pg("SELECT pub|lic.users.name FROM public.users").1, None);
}

#[test]
fn relation_in_from_clause() {
    let (shown, t) = pg("SELECT * FROM us|ers");
    assert_eq!(shown, "users");
    assert_eq!(
        t,
        Some(HoverTarget::Relation(rel(
            Some("public"),
            "users",
            RelationKind::Table
        )))
    );
    let (_, t) = pg("SELECT * FROM public.us|ers");
    assert_eq!(
        t,
        Some(HoverTarget::Relation(rel(
            Some("public"),
            "users",
            RelationKind::Table
        )))
    );
    assert_eq!(
        pg("SELECT * FROM pub|lic.users").1,
        None,
        "schema names show nothing"
    );
    let (_, t) = pg("SELECT * FROM active_us|ers");
    assert_eq!(
        t,
        Some(HoverTarget::Relation(rel(
            Some("public"),
            "active_users",
            RelationKind::View
        )))
    );
    let (_, t) = pg("SELECT * FROM reporting.tot|als");
    assert_eq!(
        t,
        Some(HoverTarget::Relation(rel(
            Some("reporting"),
            "totals",
            RelationKind::MaterializedView
        )))
    );
}

#[test]
fn relation_through_alias() {
    for sql in [
        "SELECT * FROM users |u",
        "SELECT * FROM users AS |u",
        "SELECT |u.id FROM users u",
        "SELECT u.id FROM users u WHERE |u.id = 1",
    ] {
        assert_eq!(
            pg(sql).1,
            Some(HoverTarget::Relation(rel(
                Some("public"),
                "users",
                RelationKind::Table
            ))),
            "{sql}"
        );
    }
}

#[test]
fn ctes_and_subqueries() {
    let cte = Some(HoverTarget::Relation(rel(None, "c", RelationKind::Cte)));
    assert_eq!(
        pg("WITH |c AS (SELECT id FROM users) SELECT * FROM c").1,
        cte
    );
    assert_eq!(
        pg("WITH c AS (SELECT id FROM users) SELECT * FROM |c").1,
        cte
    );
    assert_eq!(
        pg("WITH c AS (SELECT id FROM users) SELECT |c.id FROM c").1,
        cte
    );
    assert_eq!(
        pg("WITH c AS (SELECT id FROM users) SELECT * FROM c |x")
            .1
            .map(|t| matches!(t, HoverTarget::Relation(r) if r.kind == RelationKind::Cte)),
        Some(true)
    );
    let (shown, t) = pg("WITH c AS (SELECT id AS k FROM users) SELECT c.|k FROM c");
    assert_eq!(shown, "k");
    assert_eq!(
        t,
        Some(HoverTarget::DerivedColumn {
            relation: rel(None, "c", RelationKind::Cte),
            name: "k".to_owned()
        })
    );
    let (_, t) = pg("SELECT s.|k FROM (SELECT id AS k FROM users) s");
    assert_eq!(
        t,
        Some(HoverTarget::DerivedColumn {
            relation: rel(None, "s", RelationKind::Subquery),
            name: "k".to_owned()
        })
    );
    assert_eq!(
        pg("SELECT |s.k FROM (SELECT id AS k FROM users) s").1,
        Some(HoverTarget::Relation(rel(
            None,
            "s",
            RelationKind::Subquery
        )))
    );
    // Columns of a CTE that cannot be named.
    assert_eq!(
        pg("WITH c AS (SELECT * FROM users) SELECT c.|x FROM c").1,
        None
    );
}

#[test]
fn insert_column_list_and_update_set() {
    let (shown, t) = pg("INSERT INTO orders (id, user_|id) VALUES (1, 2)");
    assert_eq!(shown, "user_id");
    let (relation, info, fk) = column(t);
    assert_eq!(
        (relation.name.as_str(), info.name.as_str()),
        ("orders", "user_id")
    );
    assert!(fk.is_some());
    let (_, t) = pg("UPDATE orders SET tot|al = 1");
    assert_eq!(column(t).1.name, "total");
}

#[test]
fn nothing_for_keywords_literals_and_punctuation() {
    for sql in [
        "SEL|ECT id FROM users",
        "SELECT id FR|OM users",
        "SELECT 4|2 FROM users",
        "SELECT 'na|me' FROM users",
        "SELECT id /* na|me */ FROM users",
        "SELECT id -- na|me\nFROM users",
        "SELECT id |, name FROM users",
        "SELECT |(id) FROM users",
        "SELECT cou|nt(*) FROM users",
        "SELECT id AS fo|o FROM users",
        "SELECT unknown_|col FROM users",
        "SELECT x.na|me FROM users u",
        "SELECT * FROM nope|",
        "|",
    ] {
        let (shown, h) = pg(sql);
        assert_eq!(h, None, "{sql} -> {shown}");
    }
    // Columns of a relation that is not loaded.
    assert_eq!(pg("SELECT p.fo|o FROM pending p").1, None);
}

#[test]
fn offset_at_the_end_of_a_word() {
    let (shown, t) = pg("SELECT u.name| FROM users u");
    assert_eq!(shown, "name");
    assert_eq!(column(t).1.name, "name");
    // On the dot: the name before it.
    let (shown, t) = pg("SELECT u|.name FROM users u");
    assert_eq!(shown, "u");
    assert!(matches!(t, Some(HoverTarget::Relation(_))));
    // Between two tokens with a gap: nothing.
    assert_eq!(pg("SELECT u.name |FROM users u").1, None);
}

#[test]
fn multi_statement_text_uses_the_statement_under_the_cursor() {
    let sql = "SELECT id FROM users; SELECT tot|al FROM orders; SELECT name FROM users";
    let (_, t) = pg(sql);
    assert_eq!(column(t).0.name, "orders");
    // The first statement does not see the second one's tables.
    assert_eq!(pg("SELECT to|tal FROM users; SELECT 1 FROM orders").1, None);
}

#[test]
fn quoting_and_case_in_three_dialects() {
    let (shown, t) = pg("SELECT \"Mi|xed\" FROM users");
    assert_eq!(shown, "\"Mixed\"");
    assert_eq!(column(t).1.name, "Mixed");
    // Lenient: different case still finds the column.
    let (_, t) = pg("SELECT U.mix|ed FROM USERS u");
    assert_eq!(column(t).1.name, "Mixed");

    let (shown, h) = at("SELECT `u`.`na|me` FROM `users` `u`", Dialect::MySql);
    assert_eq!(shown, "`name`");
    let (relation, info, _) = column(h.map(|h| h.target));
    assert_eq!(relation, rel(Some("shop"), "users", RelationKind::Table));
    assert_eq!(info.name, "name");

    let (shown, h) = at("SELECT [u].[na|me] FROM [users] [u]", Dialect::Sqlite);
    assert_eq!(shown, "[name]");
    assert_eq!(
        column(h.map(|h| h.target)).0.schema.as_deref(),
        Some("main")
    );
    let (_, h) = at(
        "SELECT \"u\".\"na|me\" FROM \"USERS\" AS \"u\"",
        Dialect::Sqlite,
    );
    assert_eq!(column(h.map(|h| h.target)).1.name, "name");
}

#[test]
fn hover_range_includes_quotes() {
    let text = "SELECT \"name\" FROM users";
    let h = hover(
        &HoverRequest::new(text, 10, Dialect::Postgres),
        &schema(Dialect::Postgres),
    )
    .expect("hover");
    assert_eq!(h.range, 7..13);
}

#[test]
fn offsets_past_the_end_and_inside_characters() {
    let text = "SELECT 'é' FROM users";
    let s = schema(Dialect::Postgres);
    for offset in 0..text.len() + 10 {
        let _ = hover(&HoverRequest::new(text, offset, Dialect::Postgres), &s);
    }
    assert!(hover(&HoverRequest::new("", 5, Dialect::Postgres), &s).is_none());
}

#[test]
fn set_operation_branch_hovers_its_own_table() {
    // Both branches name `id`/`total`; each hovers the table of its own branch.
    let (shown, t) = pg("SELECT id FROM users UNION SELECT |user_id FROM orders");
    assert_eq!(shown, "user_id");
    let (relation, info, _) = column(t);
    assert_eq!(relation.name, "orders");
    assert_eq!(info.name, "user_id");

    let (_, t) = pg("SELECT |id FROM users UNION SELECT id FROM orders");
    assert_eq!(column(t).0.name, "users");
    let (_, t) = pg("SELECT id FROM users UNION SELECT |id FROM orders");
    assert_eq!(column(t).0.name, "orders");

    // A column that only the other branch has does not resolve.
    let (_, t) = pg("SELECT id FROM users UNION SELECT |name FROM orders");
    assert_eq!(t, None);

    // An alias defined in the other branch does not resolve.
    let (_, t) = pg("SELECT u.id FROM users u UNION SELECT o.i|d FROM orders o");
    assert_eq!(column(t).0.name, "orders");
    let (_, t) = pg("SELECT o.i|d FROM users u UNION SELECT o.id FROM orders o");
    assert_eq!(t, None);
}
