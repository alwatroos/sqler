// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Adversarial false-positive suite: realistic SQL that must not be flagged, and true positives
//! that must be. Diagnostics prefer silence: a construct the scanner does not understand must
//! never produce a diagnostic.

use sqler_completion::{Diagnostic, MemorySchema, diagnose};
use sqler_core::{ColumnInfo, DataType, ObjectKind};
use sqler_sql::Dialect;

#[allow(clippy::unnecessary_wraps)]
fn cols(n: &[&str]) -> Option<Vec<ColumnInfo>> {
    Some(
        n.iter()
            .map(|c| ColumnInfo::new(*c, DataType::Integer))
            .collect(),
    )
}

fn schema(h: &str) -> MemorySchema {
    let mut s = MemorySchema::new([h]);
    s.add_table(
        h,
        "users",
        ObjectKind::Table,
        cols(&["id", "name", "email", "meta", "created_at"]),
    );
    s.add_table(
        h,
        "orders",
        ObjectKind::Table,
        cols(&["id", "user_id", "total", "status", "created_at"]),
    );
    s.add_table(
        h,
        "items",
        ObjectKind::Table,
        cols(&["id", "order_id", "sku", "qty"]),
    );
    s
}

fn d(sql: &str, dia: Dialect) -> Vec<String> {
    let h = match dia {
        Dialect::MySql => "shop",
        Dialect::Sqlite => "main",
        _ => "public",
    };
    diagnose(sql, dia, &schema(h))
        .into_iter()
        .map(|x: Diagnostic| format!("{:?}@{:?}={}", x.code, x.range, &sql[x.range.clone()]))
        .collect()
}

const COMMON: &[&str] = &[
    "SELECT u.id, u.name FROM users u WHERE u.id IN (SELECT user_id FROM orders WHERE total > 10)",
    "WITH t AS (SELECT user_id, sum(total) AS s FROM orders GROUP BY user_id) SELECT u.name, t.s FROM users u JOIN t ON t.user_id = u.id ORDER BY t.s DESC",
    "WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM r WHERE n < 5) SELECT n FROM r",
    "SELECT id, row_number() OVER (PARTITION BY user_id ORDER BY created_at DESC ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) AS rn FROM orders",
    "SELECT o.id, rank() OVER w FROM orders o WINDOW w AS (PARTITION BY o.user_id ORDER BY o.total)",
    "SELECT count(*) AS c, status FROM orders GROUP BY status HAVING count(*) > 1 ORDER BY c",
    "SELECT u.name, (SELECT max(o.total) FROM orders o WHERE o.user_id = u.id) AS mx FROM users u",
    "SELECT * FROM users u LEFT JOIN orders o ON o.user_id = u.id AND o.status = 'x' WHERE o.id IS NULL",
    "SELECT x.a FROM (SELECT id AS a FROM users) x",
    "SELECT x.a FROM (SELECT id AS a FROM users) AS x WHERE x.a > 1",
    "SELECT CASE WHEN total > 5 THEN 'big' ELSE 'small' END AS sz, cast(total AS integer) FROM orders",
    "SELECT EXISTS (SELECT 1 FROM orders o WHERE o.user_id = users.id) FROM users",
    "INSERT INTO users (id, name) VALUES (1, 'a')",
    "INSERT INTO users (id, name) SELECT id, name FROM users",
    "UPDATE orders SET total = total + 1, status = 'y' WHERE user_id = 3",
    "DELETE FROM orders WHERE user_id IN (SELECT id FROM users WHERE email LIKE '%x')",
    "SELECT coalesce(name, 'n'), upper(email), extract(year FROM created_at) FROM users",
    "SELECT u.id FROM users u UNION SELECT user_id FROM orders ORDER BY 1",
    "SELECT * FROM users; SELECT * FROM orders; -- trailing comment\n",
    "SELECT 'it''s', 1.5e3, name /* c */ FROM users -- x",
    "SELECT i.sku FROM items i JOIN orders o USING (id)",
    "SELECT * FROM users NATURAL JOIN orders",
    "SELECT u.* , o.* FROM users u, orders o",
    "SELECT name AS \"Full Name\" FROM users ORDER BY \"Full Name\"",
    "SELECT date_trunc('day', created_at) AS day, count(*) FROM orders GROUP BY day",
    "SELECT * FROM users WHERE id BETWEEN 1 AND 3 AND name IS NOT NULL",
    "SELECT sum(total) FILTER (WHERE status = 'a') FROM orders",
    "SELECT * FROM generate_series(1,3) g",
    "SELECT * FROM users LIMIT 5 OFFSET 2",
];

const PG: &[&str] = &[
    "SELECT meta->>'a', meta->'b'->>'c', meta #>> '{a,b}', meta @> '{}'::jsonb, meta ? 'k' FROM users",
    "SELECT u.id, l.x FROM users u CROSS JOIN LATERAL (SELECT o.total AS x FROM orders o WHERE o.user_id = u.id LIMIT 1) l",
    "SELECT u.id, l.total FROM users u LEFT JOIN LATERAL (SELECT * FROM orders o WHERE o.user_id = u.id) l ON true",
    "SELECT id::text, name::varchar(10), created_at AT TIME ZONE 'UTC' FROM users",
    "SELECT * FROM users u, jsonb_array_elements(u.meta->'xs') AS e(val) WHERE e.val IS NOT NULL",
    "SELECT * FROM unnest(ARRAY[1,2]) AS t(n)",
    "INSERT INTO users (id, name) VALUES (1,'a') ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name RETURNING id, name",
    "UPDATE orders o SET total = i.qty FROM items i WHERE i.order_id = o.id RETURNING o.id",
    "DELETE FROM orders USING users u WHERE u.id = orders.user_id AND u.name = 'x'",
    "SELECT DISTINCT ON (user_id) user_id, total FROM orders ORDER BY user_id, total DESC",
    "WITH ins AS (INSERT INTO users (id) VALUES (1) RETURNING id) SELECT id FROM ins",
    "SELECT array_agg(name ORDER BY id) , string_agg(name, ',') FROM users",
    "SELECT * FROM users WHERE name ILIKE 'a%' AND email ~ 'x' AND id = ANY($1)",
    "SELECT current_date, current_timestamp, now(), session_user FROM users",
    "SELECT * FROM pg_catalog.pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace",
    "SELECT * FROM information_schema.tables WHERE table_schema = 'public'",
    "CREATE TABLE t2 (id serial PRIMARY KEY, name text NOT NULL DEFAULT 'x', u int REFERENCES users(id)); CREATE INDEX i ON t2 (name); ALTER TABLE t2 ADD COLUMN z int; DROP TABLE t2;",
    "CREATE TABLE t3 AS SELECT id FROM users; SELECT id FROM t3",
    "CREATE VIEW v AS SELECT id FROM users; SELECT id FROM v",
    "BEGIN; SET search_path TO public; COMMIT;",
    "EXPLAIN ANALYZE SELECT * FROM users",
    "SELECT $$body$$, E'a\\'b', U&'x' FROM users",
    "SELECT u.name FROM public.users AS u WHERE u.id = 1 FOR UPDATE SKIP LOCKED",
    "SELECT * FROM users TABLESAMPLE SYSTEM (10)",
    "SELECT (u).id FROM users u",
    "SELECT row(1,2), u FROM users u",
    "SELECT ARRAY(SELECT id FROM orders), id FROM users",
    "SELECT * FROM users WHERE (id, name) IN ((1,'a'))",
    "SELECT tstzrange(created_at, now()) && tstzrange(now(), now()) FROM users",
    "SELECT * FROM ROWS FROM (generate_series(1,2)) AS r(x)",
    "SELECT count(*) OVER () FROM users",
    "SELECT * FROM users GROUP BY GROUPING SETS ((id),(name))",
    "COPY users (id, name) FROM STDIN",
];

const MY: &[&str] = &[
    "SELECT `id`, `name` FROM `users` WHERE `id` = 1",
    "SELECT meta->>'$.a', JSON_EXTRACT(meta, '$.b') FROM users",
    "SELECT * FROM shop.users u WHERE u.name = \"x\"",
    "INSERT INTO users SET id = 1, name = 'x' ON DUPLICATE KEY UPDATE name = VALUES(name)",
    "UPDATE users u JOIN orders o ON o.user_id = u.id SET u.name = 'x' WHERE o.total > 1",
    "DELETE o FROM orders o JOIN users u ON u.id = o.user_id",
    "SELECT SQL_CALC_FOUND_ROWS id FROM users LIMIT 3",
    "SELECT @v := id, @@version, @x FROM users",
    "SELECT IFNULL(name, 'x'), GROUP_CONCAT(name SEPARATOR ',') FROM users",
    "SHOW TABLES; USE shop; SELECT 1",
    "CREATE TABLE t2 (id INT AUTO_INCREMENT PRIMARY KEY, n VARCHAR(10), KEY k (n)) ENGINE=InnoDB; ",
    "SELECT * FROM users FORCE INDEX (PRIMARY) WHERE id = 1",
    "SELECT USERS.ID FROM USERS",
    "SELECT * FROM users WHERE id = 1 LIMIT 1, 2",
    "SELECT u.id FROM users u WHERE u.created_at > NOW() - INTERVAL 1 DAY",
];

const SL: &[&str] = &[
    "SELECT name FROM users WHERE name = \"x\"",
    "SELECT \"name\" FROM \"users\"",
    "SELECT [name] FROM [users]",
    "SELECT json_extract(meta, '$.a'), meta->>'b', meta->'c' FROM users",
    "INSERT OR REPLACE INTO users (id, name) VALUES (1,'a')",
    "INSERT INTO users (id) VALUES (1) ON CONFLICT(id) DO UPDATE SET name = excluded.name",
    "SELECT rowid, _rowid_, oid, id FROM users",
    "SELECT * FROM sqlite_master WHERE type = 'table'",
    "SELECT * FROM sqlite_schema; PRAGMA table_info(users)",
    "CREATE TABLE t(a INTEGER PRIMARY KEY AUTOINCREMENT, b TEXT); CREATE INDEX ib ON t(b); SELECT a FROM t",
    "CREATE VIRTUAL TABLE ft USING fts5(a, b); SELECT a FROM ft WHERE ft MATCH 'x'",
    "SELECT * FROM main.users",
    "SELECT * FROM users WHERE name GLOB 'a*' AND id NOT NULL",
    "SELECT group_concat(name), total(id) FROM users",
    "SELECT * FROM pragma_table_info('users')",
    "WITH x(a) AS (VALUES (1),(2)) SELECT a FROM x",
    "SELECT 1 FROM users u INDEXED BY idx WHERE u.id=1",
    "SELECT USERS.ID FROM USERS",
    "REPLACE INTO users (id) VALUES (1)",
    "SELECT * FROM users WHERE rowid IN (SELECT rowid FROM orders)",
];

fn clean(dia: Dialect, lists: &[&[&str]]) {
    let mut bad = vec![];
    for l in lists {
        for sql in *l {
            let r = d(sql, dia);
            if !r.is_empty() {
                bad.push(format!("{dia:?}: {sql}\n   {r:?}"));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "false positives ({}):\n{}",
        bad.len(),
        bad.join("\n")
    );
}

#[test]
fn pg_clean() {
    clean(Dialect::Postgres, &[COMMON, PG]);
}
#[test]
fn my_clean() {
    clean(Dialect::MySql, &[COMMON, MY]);
}
#[test]
fn sl_clean() {
    clean(Dialect::Sqlite, &[COMMON, SL]);
}

#[test]
fn true_positives() {
    let cases: &[(&str, &str)] = &[
        ("SELECT nope FROM users", "nope"),
        ("SELECT u.nope FROM users u", "nope"),
        ("SELECT * FROM userz", "userz"),
        ("SELECT * FROM nosuch.users", "nosuch"),
        ("SELECT x.id FROM users u", "x"),
        (
            "WITH t AS (SELECT id FROM users) SELECT t.zzz FROM t",
            "zzz",
        ),
        ("SELECT s.zzz FROM (SELECT id FROM users) s", "zzz"),
        ("UPDATE orders SET nope = 1", "nope"),
        ("SELECT id FROM users; SELECT bad FROM orders", "bad"),
        (
            "SELECT * FROM users u JOIN orders o ON o.nope = u.id",
            "nope",
        ),
    ];
    for dia in Dialect::ALL {
        for (sql, want) in cases {
            let r = diagnose(
                sql,
                dia,
                &schema(match dia {
                    Dialect::MySql => "shop",
                    Dialect::Sqlite => "main",
                    _ => "public",
                }),
            );
            assert!(
                r.iter().any(|x| &sql[x.range.clone()] == *want),
                "{dia:?} {sql}: {r:?}"
            );
            assert!(r.iter().all(|x| x.range.end <= sql.len()
                && sql.is_char_boundary(x.range.start)
                && sql.is_char_boundary(x.range.end)));
        }
    }
}

const PG2: &[&str] = &[
    "CREATE TEMP TABLE tmp (a int, b int); INSERT INTO tmp VALUES (1,2); SELECT a, b FROM tmp",
    "CREATE TABLE IF NOT EXISTS s1.t (a int); SELECT a FROM s1.t",
    "CREATE SCHEMA s2; CREATE TABLE s2.t (a int); SELECT a FROM s2.t",
    "CREATE MATERIALIZED VIEW mv AS SELECT id FROM users; SELECT id FROM mv",
    "CREATE TABLE t4 (LIKE users); SELECT id FROM t4",
    "SELECT id FROM users; DROP TABLE users; SELECT 1",
    "ALTER TABLE users ADD COLUMN zz int; SELECT zz FROM users",
    "ALTER TABLE users RENAME COLUMN name TO nm; SELECT nm FROM users",
    "CREATE FUNCTION f(a int) RETURNS int AS $$ SELECT a + 1 $$ LANGUAGE sql; SELECT f(1)",
    "DO $$ BEGIN PERFORM 1; END $$",
    "SELECT \"Users\".id FROM \"users\" AS \"Users\"",
    "SELECT USERS.ID FROM Users",
    "SELECT u.id FROM users u WHERE u.id = (SELECT max(id) FROM users x WHERE x.id < u.id)",
    "SELECT a.id, b.id FROM users a JOIN users b ON a.id < b.id",
    "SELECT sub.n FROM (SELECT count(*) AS n FROM orders) sub",
    "SELECT sub.count FROM (SELECT count(*) FROM orders) sub",
    "SELECT t.* FROM (VALUES (1,'a')) AS t(x,y)",
    "SELECT t.x FROM (VALUES (1,'a')) AS t(x,y)",
    "WITH a AS (SELECT 1 AS x), b AS (SELECT x + 1 AS y FROM a) SELECT y FROM b",
    "WITH a(p,q) AS (SELECT id, name FROM users) SELECT p, q FROM a",
    "SELECT o.id FROM orders o WHERE NOT EXISTS (SELECT 1 FROM items i WHERE i.order_id = o.id)",
    "SELECT u.name, jsonb_each(u.meta) FROM users u",
    "SELECT * FROM users u, LATERAL jsonb_each_text(u.meta) AS kv(k, v) WHERE kv.k = 'a'",
    "SELECT * FROM users u, LATERAL (SELECT sum(total) AS s FROM orders WHERE user_id = u.id) x WHERE x.s > 1",
    "SELECT u.id FROM users u ORDER BY u.created_at NULLS LAST",
    "SELECT lag(total) OVER (PARTITION BY user_id ORDER BY created_at RANGE BETWEEN INTERVAL '1 day' PRECEDING AND CURRENT ROW) FROM orders",
    "SELECT first_value(id) OVER (ORDER BY id GROUPS BETWEEN 1 PRECEDING AND 1 FOLLOWING EXCLUDE CURRENT ROW) FROM orders",
    "SELECT interval '1 day' + created_at, timestamp with time zone '2020-01-01' FROM users",
    "SELECT id FROM users WHERE meta @? '$.a' AND meta #- '{a}' IS NOT NULL",
    "SELECT id FROM users WHERE name SIMILAR TO 'x' AND id IS DISTINCT FROM 3",
    "MERGE INTO users t USING orders o ON t.id = o.user_id WHEN MATCHED THEN UPDATE SET name = 'x' WHEN NOT MATCHED THEN INSERT (id) VALUES (o.id)",
    "SELECT u.id FROM ONLY users u",
    "SELECT * FROM users FETCH FIRST 3 ROWS ONLY",
    "SELECT unnest(string_to_array(name, ',')) AS part FROM users",
    "TRUNCATE users, orders; VACUUM ANALYZE users; ANALYZE",
    "GRANT SELECT ON users TO bob; COMMENT ON TABLE users IS 'x'",
    "LISTEN chan; NOTIFY chan, 'x'",
    "SELECT u.id FROM users AS u(i)",
    "SELECT * FROM (SELECT id FROM users) AS q(zz) WHERE zz > 1",
];
const MY2: &[&str] = &[
    "CREATE TEMPORARY TABLE tmp (a INT); SELECT a FROM tmp",
    "CREATE TABLE IF NOT EXISTS shop.t9 (a INT); SELECT a FROM t9",
    "SELECT `U`.`ID` FROM `users` `U`",
    "SELECT name FROM users WHERE id = 1 FOR UPDATE",
    "INSERT INTO users (id, name) VALUES (1,'a') ON DUPLICATE KEY UPDATE name = VALUES(name), email = new.email",
    "INSERT INTO users (id) VALUES (1) AS new ON DUPLICATE KEY UPDATE id = new.id",
    "SELECT ROW_NUMBER() OVER (ORDER BY id) rn, id FROM users",
    "SELECT * FROM users, JSON_TABLE(meta, '$[*]' COLUMNS (x INT PATH '$.x')) jt WHERE jt.x > 1",
    "SELECT STRAIGHT_JOIN id FROM users",
    "SELECT * FROM users PARTITION (p0)",
    "SELECT CONVERT(id, CHAR), CAST(id AS UNSIGNED), id DIV 2, id MOD 2, id XOR 1 FROM users",
    "SELECT * FROM information_schema.tables; SELECT * FROM mysql.user",
    "DELIMITER ;\nSELECT 1",
    "SELECT DISTINCTROW id FROM users",
    "SELECT 0x1F, b'01', _utf8mb4'x', X'ab' FROM users",
    "SELECT * FROM users WHERE MATCH(name) AGAINST('x' IN BOOLEAN MODE)",
    "SELECT * FROM users INTO OUTFILE '/tmp/x'",
    "SELECT id FROM users WHERE name = BINARY 'x' COLLATE utf8mb4_bin",
    "SET @a = 1; SET NAMES utf8mb4; SET autocommit = 0",
    "LOCK TABLES users WRITE; UNLOCK TABLES",
    "REPLACE INTO users (id) VALUES (1)",
    "SELECT current_user(), database(), version(), last_insert_id()",
    "SELECT * FROM users WHERE created_at > CURRENT_DATE - INTERVAL 7 DAY",
    "EXPLAIN SELECT * FROM users",
];
const SL2: &[&str] = &[
    "CREATE TABLE a(x, y); INSERT INTO a VALUES (1,2); SELECT x, y FROM a",
    "CREATE TEMP TABLE b(x); SELECT x FROM b",
    "CREATE TABLE c AS SELECT id AS k FROM users; SELECT k FROM c",
    "CREATE VIEW vv AS SELECT id FROM users; SELECT id FROM vv",
    "SELECT * FROM users WHERE id = ?1 AND name = :n AND email = @e AND meta = $p",
    "SELECT name FROM users ORDER BY name COLLATE NOCASE",
    "SELECT * FROM users LIMIT 2 OFFSET 1",
    "SELECT * FROM users u JOIN orders o ON o.user_id = u.id WHERE o.id IS NOT NULL",
    "SELECT json_each.value FROM users, json_each(users.meta)",
    "SELECT j.key, j.value FROM users u, json_each(u.meta) AS j",
    "ATTACH DATABASE 'x.db' AS other; SELECT * FROM other.t",
    "SELECT * FROM users NOT INDEXED",
    "SELECT sum(id) OVER (ORDER BY id ROWS UNBOUNDED PRECEDING) FROM users",
    "SELECT iif(id>1,1,0), typeof(id), length(name), substr(name,1,2), printf('%d', id) FROM users",
    "SELECT * FROM users WHERE id IN (1,2) AND name LIKE 'a%' ESCAPE '\\'",
    "SELECT x'ab', 0x10, 1e5, .5 FROM users",
    "SELECT 1 FROM (SELECT 1 AS one) s WHERE s.one = 1",
    "ALTER TABLE users ADD COLUMN zz; SELECT zz FROM users",
    "BEGIN IMMEDIATE; COMMIT; VACUUM; ANALYZE; REINDEX",
    "CREATE TRIGGER tr AFTER INSERT ON users BEGIN UPDATE orders SET total = new.id WHERE user_id = new.id; END",
    "SELECT id FROM users UNION ALL SELECT user_id FROM orders ORDER BY id",
    "SELECT id FROM users EXCEPT SELECT user_id FROM orders",
    "SELECT * FROM users WHERE name = 'x' RETURNING id",
    "DELETE FROM users WHERE id = 1 RETURNING *",
    "WITH RECURSIVE c(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM c LIMIT 5) SELECT x FROM c",
];

#[test]
fn pg_clean2() {
    clean(Dialect::Postgres, &[PG2]);
}
#[test]
fn my_clean2() {
    clean(Dialect::MySql, &[MY2]);
}
#[test]
fn sl_clean2() {
    clean(Dialect::Sqlite, &[SL2]);
}
