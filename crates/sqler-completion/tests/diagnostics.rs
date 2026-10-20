// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Diagnostics: one positive case per code, per-statement results and a regression test for every
//! suppression rule.

use std::fmt::Write as _;

use sqler_completion::{Diagnostic, DiagnosticCode, MemorySchema, Severity, diagnose};
use sqler_core::{ColumnInfo, DataType, ObjectKind};
use sqler_sql::Dialect;

use DiagnosticCode::{
    AmbiguousColumn, UnknownColumn, UnknownQualifier, UnknownRelation, UnknownSchema,
};

#[allow(clippy::unnecessary_wraps)]
fn cols(names: &[&str]) -> Option<Vec<ColumnInfo>> {
    Some(
        names
            .iter()
            .map(|n| ColumnInfo::new(*n, DataType::Integer))
            .collect(),
    )
}

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
        cols(&["id", "name", "email"]),
    );
    s.add_table(
        h,
        "orders",
        ObjectKind::Table,
        cols(&["id", "user_id", "total"]),
    );
    s.add_table(h, "pending", ObjectKind::Table, None);
    s.add_table(h, "active_users", ObjectKind::View, cols(&["id", "name"]));
    s.add_schema("archive");
    s.add_table("archive", "old_orders", ObjectKind::Table, cols(&["id"]));
    s.add_schema("lazy");
    s.mark_relations_not_loaded("lazy");
    s
}

fn run(sql: &str, d: Dialect) -> Vec<Diagnostic> {
    diagnose(sql, d, &schema(d))
}

/// `(code, the text of the range)` for PostgreSQL.
fn pg(sql: &str) -> Vec<(DiagnosticCode, String)> {
    run(sql, Dialect::Postgres)
        .into_iter()
        .map(|d| (d.code, sql[d.range].to_owned()))
        .collect()
}

fn none(sql: &str) {
    for d in Dialect::ALL {
        let got = run(sql, d);
        assert!(got.is_empty(), "{d:?}: {sql}\n{got:#?}");
    }
}

fn one(sql: &str, code: DiagnosticCode, text: &str) {
    assert_eq!(pg(sql), [(code, text.to_owned())], "{sql}");
}

#[test]
fn unknown_schema() {
    let sql = "SELECT * FROM nope.users";
    let d = run(sql, Dialect::Postgres);
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].code, UnknownSchema);
    assert_eq!(d[0].severity, Severity::Error);
    assert_eq!(&sql[d[0].range.clone()], "nope");
    assert_eq!(d[0].message, "unknown schema \"nope\"");
}

#[test]
fn unknown_relation() {
    let sql = "SELECT * FROM usrs u";
    let d = run(sql, Dialect::Postgres);
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].code, UnknownRelation);
    assert_eq!(d[0].severity, Severity::Error);
    assert_eq!(d[0].range, 14..18);
    assert_eq!(d[0].message, "unknown table or view \"usrs\"");
    let sql = "SELECT * FROM public.usrs";
    let d = run(sql, Dialect::Postgres);
    assert_eq!(d[0].message, "unknown table or view \"public.usrs\"");
    assert_eq!(&sql[d[0].range.clone()], "usrs");
}

#[test]
fn unknown_column() {
    let sql = "SELECT u.nme FROM users u";
    let d = run(sql, Dialect::Postgres);
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].code, UnknownColumn);
    assert_eq!(d[0].range, 9..12);
    assert_eq!(d[0].message, "unknown column \"nme\" in \"u\"");
    let sql = "SELECT nme FROM users";
    let d = run(sql, Dialect::Postgres);
    assert_eq!(d[0].message, "unknown column \"nme\"");
    assert_eq!(d[0].range, 7..10);
    one("SELECT * FROM users WHERE idd = 1", UnknownColumn, "idd");
    one(
        "SELECT * FROM users u JOIN orders o ON o.uid = u.id",
        UnknownColumn,
        "uid",
    );
    one("UPDATE users SET nmae = 'x'", UnknownColumn, "nmae");
    one("SELECT 1 FROM users GROUP BY nmae", UnknownColumn, "nmae");
    one("SELECT 1 FROM users ORDER BY nmae", UnknownColumn, "nmae");
    one(
        "SELECT public.users.nmae FROM public.users",
        UnknownColumn,
        "nmae",
    );
    one(
        "SELECT * FROM active_users WHERE emal = 1",
        UnknownColumn,
        "emal",
    );
}

#[test]
fn unknown_qualifier() {
    let sql = "SELECT x.id FROM users u";
    let d = run(sql, Dialect::Postgres);
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].code, UnknownQualifier);
    assert_eq!(d[0].range, 7..8);
    assert_eq!(d[0].message, "unknown table or alias \"x\"");
    // Like completion, the bare table name still resolves when the table has an alias.
    none("SELECT users.id FROM users u");
}

#[test]
fn ambiguous_column() {
    let sql = "SELECT id FROM users u JOIN orders o ON o.user_id = u.id";
    let d = run(sql, Dialect::Postgres);
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].code, AmbiguousColumn);
    assert_eq!(d[0].severity, Severity::Warning);
    assert_eq!(d[0].range, 7..9);
    assert_eq!(d[0].message, "column \"id\" is ambiguous (\"u\", \"o\")");
}

#[test]
fn per_statement_in_multi_statement_text() {
    let sql =
        "SELECT nme FROM users; SELECT id FROM orders; SELECT * FROM nope; SELECT x.id FROM users";
    let got = pg(sql);
    assert_eq!(
        got,
        [
            (UnknownColumn, "nme".to_owned()),
            (UnknownRelation, "nope".to_owned()),
            (UnknownQualifier, "x".to_owned()),
        ]
    );
    let d = run(sql, Dialect::Postgres);
    assert!(d.windows(2).all(|w| w[0].range.start <= w[1].range.start));
    // An unfinished statement does not disturb its neighbours.
    assert_eq!(
        pg("SELECT FROM ; SELECT nme FROM users;;"),
        [(UnknownColumn, "nme".to_owned())]
    );
}

#[test]
fn not_loaded_data_is_never_flagged() {
    // Columns of a relation that are not loaded.
    none("SELECT p.anything, whatever FROM pending p");
    none("SELECT pending.x FROM pending");
    none("INSERT INTO pending (a, b) VALUES (1, 2)");
    none("UPDATE pending SET a = 1 WHERE b = 2");
    // Relations of a schema that are not loaded.
    none("SELECT * FROM lazy.things t WHERE t.x = 1");
    // One unknown relation hides the unqualified names of the whole level.
    none("SELECT whatever FROM users u JOIN pending p ON true");
    // A schema that exists but holds no such table is a real error.
    assert_eq!(
        pg("SELECT * FROM archive.users"),
        [(UnknownRelation, "users".to_owned())]
    );
}

#[test]
fn ctes() {
    // `*` in a CTE: columns not known.
    none("WITH c AS (SELECT * FROM users) SELECT c.whatever, anything FROM c");
    // Named columns are known.
    one(
        "WITH c AS (SELECT id AS k FROM users) SELECT c.j FROM c",
        UnknownColumn,
        "j",
    );
    none("WITH c AS (SELECT id AS k FROM users) SELECT c.k, k FROM c");
    none("WITH c (a, b) AS (SELECT 1, 2) SELECT a, c.b FROM c");
    // Unnamed expressions: unknown.
    none("WITH c AS (SELECT count(*) FROM users) SELECT n FROM c");
    // A CTE name is not an unknown table, in the main query and in a subquery.
    none("WITH c AS (SELECT 1 AS k) SELECT k FROM c WHERE k IN (SELECT k FROM c)");
    // Recursive CTE refers to itself.
    none(
        "WITH RECURSIVE r (n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM r WHERE n < 5) SELECT n FROM r",
    );
    none(
        "WITH RECURSIVE r AS (SELECT 1 AS n UNION ALL SELECT r.n + 1 FROM r WHERE r.n < 5) SELECT n FROM r",
    );
    // The name of a missing relation inside a CTE is still reported.
    one(
        "WITH c AS (SELECT * FROM nope) SELECT * FROM c",
        UnknownRelation,
        "nope",
    );
}

#[test]
fn subqueries() {
    none("SELECT s.k FROM (SELECT id AS k FROM users) s");
    one(
        "SELECT s.j FROM (SELECT id AS k FROM users) s",
        UnknownColumn,
        "j",
    );
    none("SELECT s.anything FROM (SELECT * FROM users) s");
    none("SELECT x FROM generate_series(1, 3) g(x)");
    none("SELECT g.x FROM generate_series(1, 3) g");
    none("SELECT x FROM unnest(arr)");
    none("SELECT v.a FROM (VALUES (1, 2)) v (a, b)");
    // Correlated subquery sees the outer relations.
    none("SELECT * FROM users u WHERE EXISTS (SELECT 1 FROM orders o WHERE o.user_id = u.id)");
    none(
        "SELECT * FROM users u WHERE EXISTS (SELECT 1 FROM orders o WHERE user_id = u.id AND u.name = 'x')",
    );
    one(
        "SELECT * FROM users u WHERE EXISTS (SELECT 1 FROM orders o WHERE o.user_id = u.idd)",
        UnknownColumn,
        "idd",
    );
    // A parenthesised join cannot be named: be silent.
    none("SELECT a.whatever FROM (users a JOIN orders b ON a.id = b.user_id)");
}

#[test]
fn order_by_output_alias() {
    none("SELECT name AS n FROM users ORDER BY n");
    none("SELECT name AS n FROM users GROUP BY n HAVING n > 'a'");
    none("SELECT id, count(*) AS cnt FROM users GROUP BY id ORDER BY cnt DESC");
    none("SELECT id FROM users u JOIN orders o USING (id) ORDER BY id");
    // But not in WHERE.
    one(
        "SELECT name AS n FROM users WHERE n = 'x'",
        UnknownColumn,
        "n",
    );
}

#[test]
fn syntax_the_scanner_does_not_model() {
    none("SELECT extract(year FROM email) FROM users");
    none("SELECT id::int, name::varchar(10), CAST(id AS bigint) FROM users");
    none("SELECT DATE '2020-01-01', TIMESTAMP '2020-01-01 00:00:00', INTERVAL '1 day'");
    none("SELECT count(*), max(id), now() FROM users");
    none("SELECT * FROM users WHERE name = $1 AND id = ? AND email = :email AND id = @v");
    none("SELECT trim(BOTH 'x' FROM name) FROM users");
    none("SELECT CASE WHEN id > 1 THEN name ELSE email END label FROM users");
    none("SELECT id total FROM users");
    none("SELECT * FROM users WHERE id BETWEEN 1 AND 2 AND name IS NOT NULL");
    none("SELECT * FROM users WHERE name ILIKE 'a%' AND id IN (1, 2, 3)");
    none("SELECT row_number() OVER (PARTITION BY name ORDER BY id) FROM users");
    none("SELECT sum(id) OVER w FROM users WINDOW w AS (ORDER BY id)");
    none(
        "SELECT sum(id) OVER (ORDER BY id ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) FROM users",
    );
    none("SELECT name FROM users ORDER BY name DESC NULLS LAST");
    none("SELECT name COLLATE \"C\" FROM users");
    none("SELECT current_date, current_timestamp, current_user, true, NULL");
    none("SELECT * FROM users FOR UPDATE");
    none("SELECT * FROM users LIMIT 10 OFFSET 5");
    none("EXPLAIN SELECT * FROM users");
    none("EXPLAIN (FORMAT JSON) SELECT id FROM users");
    none("TABLE users");
    none("VALUES (1, 2), (3, 4)");
    none("SELECT 1");
    none("SELECT a.b.c.d FROM users a");
    none("SELECT (u).x, to_jsonb(u) FROM users u");
}

#[test]
fn insert_forms() {
    none("INSERT INTO users (id, name) VALUES (1, 'a')");
    one(
        "INSERT INTO users (id, nme) VALUES (1, 'a')",
        UnknownColumn,
        "nme",
    );
    none("INSERT INTO users (id, name) SELECT id, name FROM users");
    // The target is not in scope of the SELECT, so the same column in both is not ambiguous.
    none("INSERT INTO users (id) SELECT id FROM orders");
    none("INSERT INTO users (id) (SELECT id FROM orders)");
    none("INSERT INTO users (id) VALUES (1) RETURNING id, name");
    none(
        "INSERT INTO users (id) VALUES (1) ON CONFLICT (id) DO UPDATE SET name = excluded.name, nope = 1",
    );
    none("INSERT INTO users (id) VALUES (1) ON DUPLICATE KEY UPDATE name = VALUES(name), nope = 2");
    none("INSERT INTO users SET name = 'x'");
}

#[test]
fn delete_and_update_forms() {
    none("DELETE FROM users WHERE id = 1");
    none("DELETE FROM users USING orders WHERE users.id = orders.user_id AND total > 1");
    one(
        "DELETE FROM users USING orders WHERE users.id = orders.uid",
        UnknownColumn,
        "uid",
    );
    none("UPDATE users u SET name = o.total FROM orders o WHERE o.user_id = u.id");
    none("UPDATE users SET name = 'x' WHERE id IN (SELECT user_id FROM orders)");
    none("UPDATE users SET (name, email) = ('a', 'b')");
}

#[test]
fn ddl_and_other_statements_are_skipped() {
    none("CREATE TABLE t (a int, b int)");
    none("CREATE INDEX i ON nope (x)");
    none("ALTER TABLE nope ADD COLUMN x int");
    none("DROP TABLE nope");
    none("SET search_path = nope");
    none("SHOW TABLES");
    none("BEGIN; COMMIT");
    none("");
    none("   ;; ");
}

#[test]
fn tables_created_in_the_text_are_local() {
    none("CREATE TABLE fresh (a int); SELECT a, fresh.b FROM fresh");
    none(
        "CREATE TEMP TABLE IF NOT EXISTS tmp (a int); INSERT INTO tmp (a) VALUES (1); SELECT * FROM tmp t WHERE t.zz = 1",
    );
    none("CREATE OR REPLACE VIEW v AS SELECT 1; SELECT x FROM v");
    none("CREATE TABLE public.fresh (a int); SELECT * FROM public.fresh");
    // Created later in the text: still local.
    none("SELECT * FROM later; CREATE TABLE later (a int)");
    // Other names are still checked.
    one(
        "CREATE TABLE fresh (a int); SELECT * FROM nope",
        UnknownRelation,
        "nope",
    );
}

#[test]
fn system_names_are_not_flagged() {
    for sql in [
        "SELECT * FROM pg_catalog.pg_class",
        "SELECT relname FROM pg_class WHERE oid = 1",
        "SELECT * FROM information_schema.tables t WHERE t.table_name = 'x'",
    ] {
        assert!(run(sql, Dialect::Postgres).is_empty(), "{sql}");
    }
    assert_eq!(
        run("SELECT * FROM information_schema.tables", Dialect::MySql),
        []
    );
    assert_eq!(run("SELECT * FROM sqlite_master", Dialect::Sqlite), []);
    assert_eq!(run("SELECT 1 FROM dual", Dialect::MySql), []);
    assert_eq!(run("SELECT * FROM mysql.user", Dialect::MySql), []);
    assert_eq!(run("SELECT * FROM main.users", Dialect::Sqlite), []);
}

#[test]
fn using_and_natural_joins_are_not_ambiguous() {
    none("SELECT id FROM users JOIN orders USING (id)");
    none("SELECT id FROM users NATURAL JOIN orders");
    assert_eq!(
        pg("SELECT id FROM users, orders")
            .into_iter()
            .map(|x| x.0)
            .collect::<Vec<_>>(),
        [AmbiguousColumn]
    );
}

#[test]
fn qualifiers_that_are_not_tables() {
    none("SELECT * FROM users u WHERE EXISTS (SELECT 1 WHERE old.id = new.id)");
    none("SELECT public.now()");
    // A schema name in an expression position.
    none("SELECT archive.x FROM users");
}

#[test]
fn quoting_and_case_follow_the_dialect() {
    // PostgreSQL: unquoted folds to lower case, quoted is exact.
    none("SELECT USERS.ID FROM USERS");
    none("SELECT U.Name FROM Users U");
    assert_eq!(
        pg("SELECT * FROM \"Users\""),
        [(UnknownRelation, "\"Users\"".to_owned())]
    );
    assert_eq!(
        pg("SELECT \"Name\" FROM users"),
        [(UnknownColumn, "\"Name\"".to_owned())]
    );
    assert_eq!(
        pg("SELECT u.\"ID\" FROM users u"),
        [(UnknownColumn, "\"ID\"".to_owned())]
    );
    none("SELECT \"id\" FROM \"users\"");
    // MySQL and SQLite compare case-insensitively, quoted or not.
    for (d, q) in [(Dialect::MySql, '`'), (Dialect::Sqlite, '"')] {
        let sql = format!("SELECT {q}ID{q}, U.NAME FROM {q}USERS{q} u");
        assert!(run(&sql, d).is_empty(), "{d:?}");
    }
    // Brackets in SQLite.
    assert_eq!(run("SELECT [id] FROM [users]", Dialect::Sqlite), []);
    let d = run("SELECT [idd] FROM [users]", Dialect::Sqlite);
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].range, 7..12);
}

#[test]
fn sqlite_double_quoted_unknown_names_are_strings() {
    assert_eq!(run("SELECT \"hello\" FROM users", Dialect::Sqlite), []);
}

#[test]
fn quoted_names_with_odd_characters() {
    let mut s = MemorySchema::new(["public"]);
    s.add_table(
        "public",
        "My Table",
        ObjectKind::Table,
        cols(&["Weird \"col\"", "ünï"]),
    );
    let sql = "SELECT \"Weird \"\"col\"\"\", \"ünï\" FROM \"My Table\"";
    assert_eq!(diagnose(sql, Dialect::Postgres, &s), []);
    let sql = "SELECT \"Weird\" FROM \"My Table\"";
    let d = diagnose(sql, Dialect::Postgres, &s);
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].message, "unknown column \"Weird\"");
}

#[test]
fn incomplete_text_stays_quiet_or_sane() {
    none("SELECT ");
    none("SELECT * FROM ");
    none("SELECT id FROM users WHERE ");
    none("SELECT u. FROM users u");
    none("SELECT u.* FROM users u");
    none("SELECT 'unterminated");
    none("SELECT \"unterminated FROM users");
    none("-- comment only");
    none("/* open comment SELECT nope FROM nope");
    // Comments inside a statement are ignored.
    one("SELECT /* c */ nme -- x\n FROM users", UnknownColumn, "nme");
}

#[test]
fn empty_schema_source_knows_nothing() {
    let s = MemorySchema::default();
    let d = diagnose("SELECT a.b FROM t a, x.y", Dialect::Postgres, &s);
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn long_scripts_report_only_the_bad_statement() {
    let mut s = MemorySchema::new(["public"]);
    for t in 0..300 {
        s.add_table(
            "public",
            &format!("table_{t:05}"),
            ObjectKind::Table,
            cols(&["col_0", "col_1"]),
        );
    }
    let mut script = String::new();
    for i in 0..1000 {
        let _ = writeln!(
            script,
            "SELECT col_0 FROM table_{:05} WHERE col_1 = {i};",
            i % 300
        );
    }
    let at = script.len() / 2;
    let at = script[at..].find('\n').map_or(at, |p| at + p + 1);
    script.insert_str(
        at,
        "SELECT nope_col FROM table_00001 t WHERE t.col_1 = 1;\n",
    );
    let d = diagnose(&script, Dialect::Postgres, &s);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(d[0].code, UnknownColumn);
    assert_eq!(&script[d[0].range.clone()], "nope_col");
}

// ---- Review round: RETURNING, temp schemas, schema-qualified columns, routine ranges, SET ----

#[test]
fn returning_of_an_insert_sees_only_the_target() {
    for sql in [
        "INSERT INTO users (id) SELECT id FROM orders RETURNING id",
        "INSERT INTO users (id, name) SELECT o.id, 'x' FROM orders o RETURNING id, name",
    ] {
        assert!(pg(sql).is_empty(), "{sql}: {:?}", pg(sql));
    }
    // A name only the other relation has is not visible to RETURNING.
    one(
        "INSERT INTO users (id) SELECT id FROM orders RETURNING total",
        UnknownColumn,
        "total",
    );
    // UPDATE ... FROM and DELETE ... USING keep the from-items visible and ambiguity real.
    one(
        "UPDATE users SET name = 'x' FROM orders WHERE users.id = orders.user_id RETURNING id",
        AmbiguousColumn,
        "id",
    );
}

#[test]
fn postgres_temp_schemas_are_system_schemas() {
    for sql in [
        "SELECT * FROM pg_temp.t",
        "SELECT * FROM pg_temp_3.t",
        "SELECT * FROM pg_toast_temp_3.t",
    ] {
        assert!(pg(sql).is_empty(), "{sql}");
    }
    one("SELECT * FROM pg_tempx.t", UnknownSchema, "pg_tempx");
}

#[test]
fn schema_qualified_column_matches_a_from_item_without_schema() {
    assert_eq!(pg("SELECT * FROM users WHERE public.users.id = 1"), []);
    assert_eq!(pg("SELECT public.users.id FROM users"), []);
    let my = run(
        "SELECT * FROM users WHERE shop.users.id = 1",
        Dialect::MySql,
    );
    assert!(my.is_empty(), "{my:?}");
    // Still reported when no from-item has that name.
    one(
        "SELECT * FROM users WHERE public.orders.id = 1",
        UnknownQualifier,
        "orders",
    );
}

const BODY_MY: &str = "CREATE PROCEDURE p() BEGIN SELECT delimiter FROM users; UPDATE users SET name = p_x WHERE id = p_id; END";

#[test]
fn delimiter_is_a_command_only_at_the_start_of_a_line() {
    let d = run(BODY_MY, Dialect::MySql);
    assert!(d.is_empty(), "{d:?}");
    // A real command line still ends the routine.
    let sql = "DELIMITER //\nCREATE PROCEDURE p() BEGIN UPDATE users SET name = p_x; END//\nDELIMITER ;\nSELECT nme FROM users;";
    let d = run(sql, Dialect::MySql);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(&sql[d[0].range.clone()], "nme");
}

#[test]
fn routine_without_delimiter_ends_at_its_statement() {
    let pg_sql = "CREATE FUNCTION f() RETURNS int LANGUAGE sql AS $$ SELECT 1; SELECT nme FROM users $$; SELECT nme FROM users;";
    let d = run(pg_sql, Dialect::Postgres);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(d[0].range.start, pg_sql.rfind("nme").unwrap());

    let lite = "CREATE TRIGGER t AFTER INSERT ON users BEGIN UPDATE users SET name = CASE WHEN id = 1 THEN 'a' END WHERE id = new_id; INSERT INTO usrs VALUES (1); END; SELECT nme FROM users;";
    let d = run(lite, Dialect::Sqlite);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(&lite[d[0].range.clone()], "nme");

    let my = "CREATE PROCEDURE p() BEGIN IF 1 THEN SELECT 1; END IF; WHILE 1 DO SELECT 2; END WHILE; END; SELECT nme FROM users;";
    let d = run(my, Dialect::MySql);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(&my[d[0].range.clone()], "nme");
}

#[test]
fn nothing_inside_a_routine_is_reported() {
    let sql = "DELIMITER //\nCREATE PROCEDURE p() BEGIN SELECT 1; INSERT INTO usrs VALUES (1); SELECT * FROM nope.users; END//\nDELIMITER ;";
    let d = run(sql, Dialect::MySql);
    assert!(d.is_empty(), "{d:?}");
    let sql = "CREATE PROCEDURE p() BEGIN SELECT 1; INSERT INTO usrs VALUES (1); END";
    let d = run(sql, Dialect::MySql);
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn long_set_with_many_case_branches_is_linear() {
    let mut sql = String::from("UPDATE users SET name = CASE");
    for i in 0..32_000 {
        let _ = write!(sql, " WHEN id = {i} THEN name");
    }
    sql.push_str(" END WHERE id > 0");
    let start = std::time::Instant::now();
    let d = run(&sql, Dialect::Postgres);
    let took = start.elapsed();
    assert!(d.is_empty(), "{:?}", d.len());
    assert!(took.as_millis() < 2000, "took {took:?}");
}

#[test]
fn set_tuple_targets_are_still_exempt_from_ambiguity() {
    let sql = "UPDATE users SET (id, name) = (1, 'x') FROM orders WHERE users.id = orders.user_id";
    assert!(pg(sql).is_empty(), "{:?}", pg(sql));
}
