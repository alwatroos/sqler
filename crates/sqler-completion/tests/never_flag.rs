// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! "Never flag" tests: valid SQL must produce no diagnostics. Diagnostics prefer silence; any
//! construct the scanner does not fully understand has to suppress, never produce, a diagnostic.
//! A generated corpus of a few thousand valid statements plus the constructs that once slipped
//! through (system columns, set operations, modifiers, in-script DDL).

use sqler_completion::{DiagnosticCode, MemorySchema, diagnose};
use sqler_core::{ColumnInfo, DataType, ObjectKind};
use sqler_sql::Dialect;

fn home(dialect: Dialect) -> &'static str {
    match dialect {
        Dialect::MySql => "shop",
        Dialect::Sqlite => "main",
        _ => "public",
    }
}

fn schema(dialect: Dialect) -> MemorySchema {
    let h = home(dialect);
    let cols = |n: &[&str]| {
        Some(
            n.iter()
                .map(|c| ColumnInfo::new(*c, DataType::Integer))
                .collect(),
        )
    };
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
        cols(&["oid_", "user_id", "total"]),
    );
    s
}

fn found(sql: &str, dialect: Dialect) -> Vec<(DiagnosticCode, String)> {
    diagnose(sql, dialect, &schema(dialect))
        .into_iter()
        .map(|d| (d.code, sql[d.range].to_owned()))
        .collect()
}

fn assert_clean(dialect: Dialect, statements: &[&str]) {
    let bad: Vec<String> = statements
        .iter()
        .filter_map(|sql| {
            let r = found(sql, dialect);
            (!r.is_empty()).then(|| format!("{dialect:?}: {sql}\n    {r:?}"))
        })
        .collect();
    assert!(bad.is_empty(), "false positives:\n{}", bad.join("\n"));
}

#[test]
fn generated_corpus_is_clean() {
    let items = [
        "id",
        "name",
        "u.id",
        "u.name",
        "u.*",
        "*",
        "count(*)",
        "coalesce(u.name, 'x')",
        "u.id + 1 AS next_id",
        "CASE WHEN u.id > 1 THEN 'a' ELSE 'b' END",
        "cast(u.id AS integer)",
        "u.id, u.email",
        "(SELECT max(o.total) FROM orders o WHERE o.user_id = u.id) AS mx",
    ];
    let froms = [
        "FROM users u",
        "FROM users AS u",
        "FROM users u LEFT JOIN orders o ON o.user_id = u.id",
        "FROM users u, orders o",
    ];
    let tails = [
        "",
        "WHERE u.id > 1",
        "WHERE u.id IN (SELECT user_id FROM orders)",
        "WHERE EXISTS (SELECT 1 FROM orders o2 WHERE o2.user_id = u.id)",
        "GROUP BY u.id",
        "ORDER BY u.id",
        "ORDER BY 1 DESC",
        "LIMIT 3",
        "UNION SELECT user_id FROM orders",
        "UNION ALL SELECT id FROM users u ORDER BY 1",
        "EXCEPT SELECT user_id FROM orders",
        "INTERSECT SELECT id FROM users",
    ];
    let wrappers = [
        "{}",
        "SELECT * FROM ({}) AS sub",
        "WITH c AS ({}) SELECT * FROM c",
        "SELECT 1 WHERE EXISTS ({})",
    ];
    for dialect in Dialect::ALL {
        let mut all: Vec<String> = Vec::new();
        for item in items {
            for from in froms {
                // `id` and `name` would be ambiguous only if both tables had them; they do not,
                // so every combination is valid.
                for tail in tails {
                    let q = format!("SELECT {item} {from} {tail}");
                    for w in wrappers {
                        all.push(w.replace("{}", &q));
                    }
                }
            }
        }
        assert!(all.len() > 500, "{} statements", all.len());
        let refs: Vec<&str> = all.iter().map(String::as_str).collect();
        assert_clean(dialect, &refs);
    }
}

const POSTGRES: &[&str] = &[
    "SELECT ctid, xmin, xmax, cmin, cmax, tableoid, id FROM users",
    "SELECT u.ctid, u.xmin, u.tableoid::regclass FROM users u",
    "SELECT * FROM users GROUP BY GROUPING SETS ((id), (name), ())",
    "SELECT id, name FROM users GROUP BY CUBE (id, name)",
    "SELECT id, name FROM users GROUP BY ROLLUP (id, name)",
    "SELECT * FROM ROWS FROM (generate_series(1, 2), generate_series(1, 3)) AS r(a, b)",
    "SELECT U&'d\\0061t\\+000061' FROM users",
    "SELECT U&\"d\\0061t\\+000061\" FROM users",
    "SELECT * FROM (SELECT id FROM users) AS q(a) WHERE a > 1",
    "SELECT q.a, q.name FROM (SELECT id, name FROM users) AS q(a)",
    "ALTER TABLE users ADD COLUMN zz int; SELECT zz FROM users",
    "ALTER TABLE users RENAME COLUMN name TO nm; SELECT nm FROM users",
    "ALTER TABLE users DROP COLUMN email; SELECT id FROM users",
    "ALTER TABLE users RENAME TO people; SELECT id FROM people",
    "ALTER TABLE IF EXISTS ONLY users ADD COLUMN zz int; SELECT u.zz FROM users u",
    "SELECT id, name INTO TEMP copy1 FROM users; SELECT id FROM copy1",
    "SELECT * INTO copy2 FROM users; SELECT * FROM copy2 c WHERE c.id = 1",
    "CREATE SCHEMA extra; SELECT * FROM extra.things",
    "UPDATE users SET id = 1 FROM orders WHERE orders.user_id = users.id",
    "UPDATE users u SET name = 'x', email = 'y' FROM orders o WHERE o.user_id = u.id",
    "UPDATE users SET (id, name) = (1, 'x') FROM orders WHERE total > 1",
    "UPDATE ONLY users SET name = 'x' WHERE id = 1",
    "SELECT id FROM users UNION SELECT user_id FROM orders",
    "SELECT * FROM users WHERE id = 1 UNION SELECT * FROM users WHERE id = 2 ORDER BY id",
    "WITH c AS (SELECT id FROM users UNION SELECT user_id FROM orders) SELECT id FROM c",
    "SELECT * FROM (SELECT id FROM users UNION SELECT oid_ FROM orders) s WHERE s.id > 1",
    "SELECT u.id FROM users u UNION SELECT u.total FROM orders u",
    "SELECT id AS x FROM users UNION SELECT user_id FROM orders ORDER BY x",
    "SELECT * FROM users TABLESAMPLE SYSTEM (10) t WHERE t.id = 1",
    "SELECT oid, id FROM users",
    "SELECT id FROM users FOR SHARE OF users NOWAIT",
    "SELECT * FROM users u JOIN orders o ON o.user_id = u.id AND o.total > ALL (SELECT 1)",
    "SELECT * FROM generate_series(1, 3) AS g(n) JOIN users u ON u.id = g.n",
    "SELECT g.n FROM generate_series(1, 3) AS g(n)",
    "SELECT * FROM users u, unnest(ARRAY[1, 2]) WITH ORDINALITY AS t(v, i) WHERE t.i = u.id",
    "SELECT id FROM users EXCEPT ALL SELECT user_id FROM orders ORDER BY 1",
    "SELECT * FROM users u WHERE u.id = ANY (SELECT user_id FROM orders UNION SELECT id FROM users)",
    "DELETE FROM users WHERE id IN (SELECT user_id FROM orders UNION SELECT id FROM users)",
    "INSERT INTO users (id, name) SELECT user_id, 'x' FROM orders UNION SELECT id, name FROM users",
    "CREATE TABLE t9 AS SELECT id FROM users; ALTER TABLE t9 ADD COLUMN k int; SELECT k FROM t9",
    "EXPLAIN (ANALYZE) SELECT id FROM users UNION SELECT user_id FROM orders",
    "SELECT * FROM users u TABLESAMPLE BERNOULLI (10) WHERE u.id = 1",
];

const MYSQL: &[&str] = &[
    "SELECT * FROM users u STRAIGHT_JOIN orders o ON o.user_id = u.id",
    "SELECT STRAIGHT_JOIN u.id FROM users u JOIN orders o ON o.user_id = u.id",
    "SELECT SQL_CALC_FOUND_ROWS id FROM users LIMIT 3",
    "SELECT SQL_NO_CACHE id FROM users",
    "SELECT SQL_CACHE id FROM users",
    "SELECT HIGH_PRIORITY id FROM users",
    "SELECT DISTINCTROW id FROM users",
    "SELECT SQL_SMALL_RESULT SQL_BUFFER_RESULT id FROM users",
    "SELECT SQL_BIG_RESULT DISTINCT name FROM users",
    "SELECT CONVERT(name USING utf8mb4) FROM users",
    "SELECT CONVERT(name USING latin1) AS n FROM users u WHERE u.id = 1",
    "SELECT u.id, _rowid FROM users u",
    "RENAME TABLE users TO people, orders TO sales; SELECT id FROM people",
    "ALTER TABLE users ADD COLUMN zz INT; SELECT zz FROM users",
    "ALTER TABLE users CHANGE name nm VARCHAR(10); SELECT nm FROM users",
    "ALTER TABLE users RENAME COLUMN name TO nm; SELECT u.nm FROM users u",
    "ALTER TABLE users DROP COLUMN email; SELECT id FROM users",
    "UPDATE LOW_PRIORITY users SET name = 'x' WHERE id = 1",
    "UPDATE IGNORE users SET id = 2 WHERE id = 1",
    "UPDATE users u JOIN orders o ON o.user_id = u.id SET u.id = 1",
    "UPDATE users, orders SET users.name = 'x' WHERE orders.user_id = users.id",
    "SELECT * FROM users USE INDEX (PRIMARY) JOIN orders ON orders.user_id = users.id",
    "SELECT * FROM users u FORCE INDEX (PRIMARY) JOIN orders o ON o.user_id = u.id",
    "DELETE QUICK FROM users WHERE id = 1",
    "DELETE LOW_PRIORITY IGNORE FROM users WHERE id = 1",
    "INSERT DELAYED INTO users (id) VALUES (1)",
    "INSERT IGNORE INTO users (id) VALUES (1)",
    "SELECT id FROM users LOCK IN SHARE MODE",
    "SELECT * FROM users u NATURAL LEFT JOIN orders",
    "SELECT id FROM users UNION SELECT user_id FROM orders",
    "(SELECT id FROM users) UNION (SELECT user_id FROM orders) ORDER BY id",
    "SELECT * FROM (SELECT id FROM users) AS q(a) WHERE a > 1",
    "SELECT * FROM (SELECT id FROM users) AS q WHERE q.id > 1 UNION SELECT user_id, 1, 1 FROM orders",
    "CREATE DATABASE extra; SELECT * FROM extra.things",
    "SELECT GROUP_CONCAT(name ORDER BY id SEPARATOR ';') FROM users GROUP BY id WITH ROLLUP",
    "UPDATE users SET id = 1 WHERE name IN (SELECT name FROM users)",
];

const SQLITE: &[&str] = &[
    "SELECT rowid, _rowid_, oid, id FROM users",
    "SELECT u.rowid, u._rowid_, u.oid FROM users u",
    "SELECT * FROM users WHERE rowid = 1",
    "SELECT main.users.rowid FROM main.users",
    "CREATE VIRTUAL TABLE ft USING fts5(a, b); SELECT a FROM ft WHERE ft MATCH 'x'",
    "CREATE VIRTUAL TABLE IF NOT EXISTS ft USING fts5(a); SELECT ft.a FROM ft",
    "ATTACH DATABASE 'x.db' AS other; SELECT * FROM other.t",
    "ATTACH 'x.db' AS other; SELECT o.a FROM other.t o",
    "ALTER TABLE users ADD COLUMN zz; SELECT zz FROM users",
    "ALTER TABLE users RENAME COLUMN name TO nm; SELECT nm FROM users",
    "ALTER TABLE users DROP COLUMN email; SELECT id FROM users",
    "ALTER TABLE users RENAME TO people; SELECT id FROM people",
    "UPDATE OR REPLACE users SET id = 1 WHERE name = 'x'",
    "UPDATE OR IGNORE users SET id = 1 WHERE name = 'x'",
    "UPDATE users SET id = 1 FROM orders WHERE orders.user_id = users.id",
    "SELECT * FROM users u INDEXED BY idx WHERE u.id = 1",
    "SELECT id FROM users UNION SELECT user_id FROM orders ORDER BY 1",
    "SELECT * FROM (SELECT id FROM users) AS q(a) WHERE a > 1",
    "UPDATE OR ROLLBACK users SET id = 1",
    "INSERT OR IGNORE INTO users (id) VALUES (1)",
    "SELECT * FROM users NOT INDEXED",
    "CREATE TABLE t9 AS SELECT id FROM users; ALTER TABLE t9 ADD COLUMN k; SELECT k FROM t9",
];

#[test]
fn postgres_valid_statements_are_clean() {
    assert_clean(Dialect::Postgres, POSTGRES);
}

#[test]
fn mysql_valid_statements_are_clean() {
    assert_clean(Dialect::MySql, MYSQL);
}

#[test]
fn sqlite_valid_statements_are_clean() {
    assert_clean(Dialect::Sqlite, SQLITE);
}

#[test]
fn ambiguity_survives_where_it_is_real() {
    use DiagnosticCode::AmbiguousColumn;
    for dialect in Dialect::ALL {
        let r = found(
            "SELECT id FROM users u JOIN orders o ON o.user_id = u.id",
            dialect,
        );
        assert!(r.is_empty(), "orders has no id column here: {r:?}");
        let r = found(
            "SELECT user_id FROM orders o JOIN orders p ON p.oid_ = o.oid_",
            dialect,
        );
        assert_eq!(r, [(AmbiguousColumn, "user_id".to_owned())], "{dialect:?}");
    }
}

#[test]
fn real_errors_survive_next_to_the_new_suppressions() {
    use DiagnosticCode::{UnknownColumn, UnknownRelation};
    for dialect in Dialect::ALL {
        assert_eq!(
            found(
                "SELECT nope FROM users UNION SELECT user_id FROM orders",
                dialect
            ),
            [(UnknownColumn, "nope".to_owned())],
            "{dialect:?}"
        );
        assert_eq!(
            found(
                "SELECT u.nope FROM users u UNION SELECT u.total FROM orders u",
                dialect
            ),
            [(UnknownColumn, "nope".to_owned())],
            "{dialect:?}"
        );
        assert_eq!(
            found("SELECT 1 FROM nope_table", dialect),
            [(UnknownRelation, "nope_table".to_owned())],
            "{dialect:?}"
        );
        assert_eq!(
            found("SELECT u.nope FROM users u, orders o", dialect),
            [(UnknownColumn, "nope".to_owned())],
            "{dialect:?}"
        );
    }
}

#[test]
fn each_set_operation_branch_resolves_against_its_own_relations() {
    use DiagnosticCode::{AmbiguousColumn, UnknownColumn, UnknownQualifier};
    for dialect in Dialect::ALL {
        // A column that only the other branch's table has is unknown here.
        assert_eq!(
            found(
                "SELECT oid_ FROM users UNION SELECT user_id FROM orders",
                dialect
            ),
            [(UnknownColumn, "oid_".to_owned())],
            "{dialect:?}"
        );
        // Same name in both branches, only one table has it: the second `name` is the error.
        let sql = "SELECT name FROM users UNION SELECT name FROM orders";
        let r = diagnose(sql, dialect, &schema(dialect));
        assert_eq!(r.len(), 1, "{dialect:?}: {r:?}");
        assert_eq!(r[0].code, UnknownColumn);
        assert_eq!(r[0].range.start, sql.rfind("name").unwrap_or(0));
        // Ambiguity inside one branch, in every set operator and every position.
        for op in ["UNION", "UNION ALL", "INTERSECT", "EXCEPT"] {
            let join = "orders o JOIN orders p ON p.oid_ = o.oid_";
            assert_eq!(
                found(
                    &format!("SELECT user_id FROM {join} {op} SELECT 1"),
                    dialect
                ),
                [(AmbiguousColumn, "user_id".to_owned())],
                "{dialect:?} {op}"
            );
            assert_eq!(
                found(
                    &format!("SELECT 1 {op} SELECT user_id FROM {join}"),
                    dialect
                ),
                [(AmbiguousColumn, "user_id".to_owned())],
                "{dialect:?} {op}"
            );
        }
        // An alias that only the other branch defines is an unknown qualifier.
        assert_eq!(
            found(
                "SELECT o.name FROM users u UNION SELECT o.name FROM orders o",
                dialect
            ),
            [
                (UnknownQualifier, "o".to_owned()),
                (UnknownColumn, "name".to_owned())
            ],
            "{dialect:?}"
        );
        assert_eq!(
            found(
                "SELECT o.total FROM users u UNION SELECT o.total FROM orders o",
                dialect
            ),
            [(UnknownQualifier, "o".to_owned())],
            "{dialect:?}"
        );
        // A schema-qualified reference sees only its own branch.
        let h = home(dialect);
        assert_eq!(
            found(
                &format!("SELECT {h}.orders.total FROM users UNION SELECT {h}.orders.total FROM {h}.orders"),
                dialect
            )
            .iter()
            .map(|d| d.0)
            .collect::<Vec<_>>(),
            [UnknownQualifier],
            "{dialect:?}"
        );
    }
}

#[test]
fn set_operation_branches_stay_silent_where_names_are_shared() {
    for dialect in Dialect::ALL {
        assert_clean(
            dialect,
            &[
                "SELECT id FROM users UNION SELECT user_id FROM orders ORDER BY id",
                "SELECT id FROM users UNION SELECT user_id FROM orders ORDER BY 1 DESC",
                "SELECT id AS k FROM users UNION ALL SELECT total FROM orders ORDER BY k LIMIT 3",
                "SELECT u.id FROM users u UNION SELECT u.user_id FROM orders u",
                "SELECT id FROM users u WHERE EXISTS (SELECT 1 FROM orders o WHERE o.user_id = u.id) UNION SELECT user_id FROM orders",
                "SELECT id FROM users UNION SELECT user_id FROM orders WHERE user_id IN (SELECT id FROM users)",
                "WITH r(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM r) SELECT n FROM r",
                "SELECT * FROM (SELECT id FROM users UNION SELECT user_id FROM orders) x WHERE x.id > 1",
                "(SELECT id FROM users) UNION (SELECT user_id FROM orders)",
            ],
        );
    }
}

#[test]
fn only_the_assignment_target_is_exempt_from_ambiguity() {
    use DiagnosticCode::AmbiguousColumn;
    let from = "FROM orders p WHERE p.oid_ = o.oid_";
    for dialect in [Dialect::Postgres, Dialect::Sqlite] {
        assert_eq!(
            found(
                &format!("UPDATE orders o SET total = user_id {from}"),
                dialect
            ),
            [(AmbiguousColumn, "user_id".to_owned())],
            "{dialect:?}"
        );
        assert_eq!(
            found(
                &format!("UPDATE orders o SET total = 1, user_id = user_id + 1 {from}"),
                dialect
            ),
            [(AmbiguousColumn, "user_id".to_owned())],
            "{dialect:?}"
        );
        assert_clean(
            dialect,
            &[
                &format!("UPDATE orders o SET user_id = 1 {from}"),
                &format!("UPDATE orders o SET (user_id, total) = (1, 2) {from}"),
                &format!("UPDATE orders o SET total = o.user_id, user_id = p.user_id {from}"),
            ],
        );
    }
}

#[test]
fn ctes_after_the_insert_target_are_in_scope() {
    use DiagnosticCode::{UnknownColumn, UnknownRelation};
    for dialect in Dialect::ALL {
        assert_clean(
            dialect,
            &[
                "INSERT INTO orders (oid_, user_id, total) WITH s AS (SELECT id FROM users WHERE id > 3) SELECT s.id, s.id, 1 FROM s",
                "INSERT INTO orders (oid_, user_id) WITH s AS (SELECT id FROM users), t AS (SELECT id FROM s) SELECT id, id FROM t",
                "INSERT INTO orders WITH s AS (SELECT id FROM users) SELECT id, id, id FROM s",
                "WITH s AS (SELECT id FROM users) INSERT INTO orders (oid_, user_id) SELECT id, id FROM s",
            ],
        );
        assert_eq!(
            found(
                "INSERT INTO orders (oid_, user_id) WITH s AS (SELECT id FROM users) SELECT s.nope, 1 FROM s",
                dialect
            ),
            [(UnknownColumn, "nope".to_owned())],
            "{dialect:?}"
        );
        assert_eq!(
            found(
                "INSERT INTO orders (oid_) WITH s AS (SELECT id FROM users) SELECT 1 FROM nope",
                dialect
            ),
            [(UnknownRelation, "nope".to_owned())],
            "{dialect:?}"
        );
    }
}

#[test]
fn postgres_search_and_cycle_columns_are_not_flagged() {
    assert_clean(
        Dialect::Postgres,
        &[
            "WITH RECURSIVE t(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM t) SEARCH BREADTH FIRST BY n SET seq SELECT * FROM t ORDER BY seq LIMIT 5",
            "WITH RECURSIVE t(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM t) SEARCH DEPTH FIRST BY n SET ordercol SELECT n, ordercol FROM t ORDER BY ordercol",
            "WITH RECURSIVE t(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM t) CYCLE n SET is_cycle USING cpath SELECT n, is_cycle, cpath FROM t WHERE NOT is_cycle",
            "WITH RECURSIVE t(n, m) AS (SELECT 1, 2 UNION ALL SELECT n + 1, m FROM t) SEARCH DEPTH FIRST BY n, m SET ordercol CYCLE n, m SET is_cycle TO true DEFAULT false USING cpath SELECT t.ordercol, t.is_cycle FROM t ORDER BY ordercol",
            "WITH RECURSIVE t(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM t) SEARCH BREADTH FIRST BY n SET seq, u AS (SELECT id FROM users) SELECT seq, u.id FROM t, u",
        ],
    );
    assert_eq!(
        found(
            "WITH RECURSIVE t(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM t) SEARCH BREADTH FIRST BY n SET seq SELECT * FROM nope",
            Dialect::Postgres
        ),
        [(DiagnosticCode::UnknownRelation, "nope".to_owned())]
    );
}

#[test]
fn share_mode_locking_words_are_not_columns() {
    for dialect in Dialect::ALL {
        assert_clean(
            dialect,
            &[
                "SELECT * FROM users u WHERE u.id = 1 LOCK IN SHARE MODE",
                "SELECT * FROM users u WHERE u.id = 1 FOR SHARE",
                "SELECT * FROM users WHERE id = 1 FOR SHARE NOWAIT",
                "SELECT * FROM users WHERE id = 1 FOR UPDATE SKIP LOCKED",
            ],
        );
    }
    assert_eq!(
        found(
            "SELECT * FROM users u WHERE u.nope = 1 LOCK IN SHARE MODE",
            Dialect::MySql
        ),
        [(DiagnosticCode::UnknownColumn, "nope".to_owned())]
    );
}
