// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Broad corpus of valid statements (reporting queries, window frames, CTEs, upserts, MERGE, JSON
//! tables, generated columns, locking clauses) that must not produce a diagnostic, plus a few
//! statements that must. Folded in from the tester's fresh probe.

use sqler_completion::{DiagnosticCode, MemorySchema, diagnose};
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
fn run(dia: Dialect, h: &str, l: &[&str]) {
    let mut bad = vec![];
    for q in l {
        let r = diagnose(q, dia, &schema(h));
        if !r.is_empty() {
            bad.push(format!(
                "{q}\n   -> {:?}",
                r.iter()
                    .map(|x| format!("{:?}@{}", x.code, &q[x.range.clone()]))
                    .collect::<Vec<_>>()
            ));
        }
    }
    assert!(bad.is_empty(), "{} FP:\n{}", bad.len(), bad.join("\n"));
}
const COMMON: &[&str] = &[
    "SELECT date_trunc('month', o.created_at) AS m, u.name, sum(o.total) AS rev, sum(sum(o.total)) OVER (PARTITION BY u.id ORDER BY date_trunc('month', o.created_at)) AS running FROM orders o JOIN users u ON u.id=o.user_id GROUP BY 1, u.id, u.name ORDER BY m, rev DESC",
    "SELECT id, total, avg(total) OVER (ORDER BY created_at ROWS BETWEEN 2 PRECEDING AND CURRENT ROW) AS ma, lag(total, 1, 0) OVER (PARTITION BY user_id ORDER BY created_at) AS prev FROM orders",
    "SELECT id, sum(total) OVER (ORDER BY id RANGE BETWEEN 5 PRECEDING AND 5 FOLLOWING) FROM orders",
    "SELECT id, ntile(4) OVER (ORDER BY total) AS q, first_value(total) OVER w AS fv, last_value(total) OVER w FROM orders WINDOW w AS (PARTITION BY user_id ORDER BY created_at ROWS BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING)",
    "WITH RECURSIVE tree(id, depth) AS (SELECT id, 0 FROM users WHERE id = 1 UNION ALL SELECT o.id, t.depth+1 FROM orders o JOIN tree t ON o.user_id = t.id WHERE t.depth < 5) SELECT * FROM tree ORDER BY depth",
    "WITH a AS (SELECT id FROM users), b AS (SELECT user_id, count(*) c FROM orders GROUP BY user_id) SELECT a.id, coalesce(b.c,0) FROM a LEFT JOIN b ON b.user_id=a.id",
    "INSERT INTO orders (id, user_id, total) WITH s AS (SELECT id FROM users WHERE id>3) SELECT s.id, s.id, 1 FROM s",
    "SELECT u.id, (SELECT count(*) FROM orders o WHERE o.user_id=u.id) AS n, (SELECT sum(i.qty) FROM items i JOIN orders o2 ON o2.id=i.order_id WHERE o2.user_id=u.id) AS q FROM users u",
    "SELECT status, count(*) FILTER (WHERE total > 10) AS big, count(DISTINCT user_id) FROM orders GROUP BY status HAVING sum(total) > 100",
    "SELECT u.name FROM users u WHERE NOT EXISTS (SELECT 1 FROM orders o WHERE o.user_id = u.id AND o.status = 'paid')",
    "SELECT o.id FROM orders o INNER JOIN items i ON i.order_id = o.id RIGHT JOIN users u ON u.id = o.user_id WHERE i.sku IS NULL",
    "SELECT t.* FROM (SELECT user_id, max(total) AS mx FROM orders GROUP BY user_id) t WHERE t.mx > 5",
    "SELECT a.id, b.id FROM users a JOIN users b ON a.email = b.email AND a.id < b.id",
    "UPDATE orders SET status = 'x' WHERE id IN (SELECT order_id FROM items WHERE qty > 3)",
    "DELETE FROM items WHERE order_id NOT IN (SELECT id FROM orders)",
    "SELECT * FROM users WHERE created_at >= '2024-01-01' AND (name LIKE 'a%' OR email LIKE '%@x.com') ORDER BY created_at DESC LIMIT 10",
    "SELECT u.id, o.total FROM users u JOIN orders o ON o.user_id = u.id ORDER BY o.total DESC, u.id",
    "SELECT user_id, total FROM orders UNION ALL SELECT id, 0 FROM users",
    "SELECT id FROM users INTERSECT SELECT user_id FROM orders",
    "SELECT x.s FROM (SELECT sku AS s FROM items) AS x(s)",
    "CREATE TABLE audit (id INT, at TIMESTAMP, msg TEXT); INSERT INTO audit (id, msg) SELECT id, name FROM users; SELECT msg FROM audit",
    "SELECT CASE status WHEN 'a' THEN 1 WHEN 'b' THEN 2 END AS k, total FROM orders ORDER BY k",
    "SELECT 1 AS one, 2 two FROM users u ORDER BY one",
    "SELECT count(*) c FROM orders GROUP BY user_id ORDER BY c DESC",
];
const PG: &[&str] = &[
    "MERGE INTO orders o USING (SELECT id, total FROM items_src) s ON o.id = s.id WHEN MATCHED THEN UPDATE SET total = s.total WHEN NOT MATCHED THEN INSERT (id, total) VALUES (s.id, s.total)",
    "MERGE INTO orders o USING users u ON o.user_id = u.id WHEN MATCHED AND o.total > 5 THEN DELETE WHEN NOT MATCHED THEN DO NOTHING",
    "INSERT INTO users (id, name, email) VALUES (1,'a','b') ON CONFLICT ON CONSTRAINT users_pkey DO NOTHING",
    "INSERT INTO users AS u (id, name) VALUES (1,'a') ON CONFLICT (id) DO UPDATE SET name = u.name || EXCLUDED.name WHERE u.id > 0 RETURNING u.id, u.name AS nm",
    "INSERT INTO users (id, name) SELECT id, name FROM users WHERE id > 100 ON CONFLICT (id) DO UPDATE SET name = excluded.name",
    "WITH RECURSIVE t(n, path) AS (SELECT id, ARRAY[id] FROM users UNION ALL SELECT o.id, t.path || o.id FROM orders o JOIN t ON o.user_id = t.n) SEARCH DEPTH FIRST BY n SET ordercol CYCLE n SET is_cycle USING cpath SELECT n, ordercol, is_cycle FROM t ORDER BY ordercol",
    "WITH RECURSIVE t(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM t) SEARCH BREADTH FIRST BY n SET seq SELECT * FROM t ORDER BY seq LIMIT 5",
    "CREATE TABLE g (a int, b int, c int GENERATED ALWAYS AS (a + b) STORED); SELECT c FROM g WHERE a > 1",
    "CREATE TABLE g2 (id int GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY, v text); INSERT INTO g2 (v) VALUES ('x') RETURNING id",
    "SELECT jt.* FROM users u, JSON_TABLE(u.meta, '$[*]' COLUMNS (k text PATH '$.k', n int PATH '$.n')) AS jt",
    "SELECT elem->>'sku' AS sku, e.ord FROM users u CROSS JOIN LATERAL jsonb_array_elements(u.meta->'items') WITH ORDINALITY AS e(elem, ord)",
    "SELECT u.id, jsonb_path_query(u.meta, '$.a[*]') FROM users u WHERE u.meta @? '$.a'",
    "SELECT name, percentile_cont(0.5) WITHIN GROUP (ORDER BY total) FROM orders o JOIN users u ON u.id = o.user_id GROUP BY name",
    "SELECT string_agg(DISTINCT sku, ', ' ORDER BY sku) FROM items GROUP BY order_id",
    "SELECT * FROM users u WHERE u.created_at BETWEEN SYMMETRIC now() - interval '1 day' AND now()",
    "SELECT d::date, count(*) FROM generate_series(now() - interval '30 days', now(), interval '1 day') AS d LEFT JOIN orders o ON o.created_at::date = d::date GROUP BY d",
    "SELECT * FROM orders WHERE (user_id, status) IN (SELECT id, 'a' FROM users)",
    "SELECT o.id FROM orders o WHERE o.total > ALL (SELECT total FROM orders WHERE user_id = 1)",
    "UPDATE users SET meta = jsonb_set(meta, '{a}', to_jsonb(name)) WHERE meta ? 'a' RETURNING *",
    "UPDATE orders SET (status, total) = ('a', 1) WHERE id = 1",
    "WITH moved AS (DELETE FROM orders WHERE total = 0 RETURNING *) INSERT INTO items (id, order_id) SELECT id, id FROM moved",
    "SELECT id FROM users FETCH FIRST 5 ROWS ONLY",
    "SELECT id FROM users ORDER BY id OFFSET 5 ROWS FETCH NEXT 5 ROWS WITH TIES",
    "SELECT * FROM users u JOIN LATERAL (SELECT * FROM orders WHERE user_id = u.id ORDER BY total LIMIT 3) top3 ON TRUE",
    "SELECT id, name FROM users WINDOW w AS (ORDER BY id) ORDER BY id",
    "CREATE MATERIALIZED VIEW mv AS SELECT user_id, sum(total) s FROM orders GROUP BY 1; SELECT s FROM mv",
    "TRUNCATE users, orders RESTART IDENTITY CASCADE",
    "SELECT x FROM (VALUES (1),(2)) AS v(x)",
    "VALUES (1, 'a'), (2, 'b')",
    "SELECT id FROM users WHERE name = ANY(ARRAY['a','b']) AND email !~* 'zz'",
    "SELECT to_char(created_at, 'YYYY-MM') AS ym, extract(epoch FROM created_at) FROM users",
    "SELECT * FROM users u WHERE u.id = (SELECT max(id) FROM users)",
    "SELECT COALESCE(NULLIF(name,''),'n/a') AS n FROM users GROUP BY ROLLUP (n)",
    "SELECT user_id, status, count(*) FROM orders GROUP BY CUBE (user_id, status)",
];
const MY: &[&str] = &[
    "INSERT INTO users (id, name) VALUES (1,'a') AS new ON DUPLICATE KEY UPDATE name = new.name",
    "INSERT INTO users (id, name) SELECT id, name FROM users u2 WHERE u2.id > 5 ON DUPLICATE KEY UPDATE name = VALUES(name)",
    "REPLACE INTO users (id, name) VALUES (1, 'a')",
    "INSERT IGNORE INTO users (id) VALUES (1)",
    "SELECT jt.* FROM users u, JSON_TABLE(u.meta, '$[*]' COLUMNS (k VARCHAR(10) PATH '$.k', n INT PATH '$.n' DEFAULT '0' ON EMPTY, rid FOR ORDINALITY)) AS jt",
    "CREATE TABLE g (a INT, b INT, c INT GENERATED ALWAYS AS (a + b) VIRTUAL, d INT AS (a*2) STORED); SELECT c, d FROM g",
    "WITH RECURSIVE t(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM t WHERE n<5) SELECT n FROM t",
    "INSERT INTO orders (id, user_id) WITH s AS (SELECT id FROM users) SELECT id, id FROM s",
    "SELECT `o`.`id`, `u`.`name` FROM `orders` AS `o` JOIN `users` AS `u` ON `u`.`id` = `o`.`user_id`",
    "SELECT u.id, SUM(o.total) OVER (PARTITION BY u.id ORDER BY o.created_at RANGE BETWEEN INTERVAL 7 DAY PRECEDING AND CURRENT ROW) FROM users u JOIN orders o ON o.user_id=u.id",
    "SELECT DATE_FORMAT(created_at, '%Y-%m') AS ym, COUNT(*) FROM orders GROUP BY ym ORDER BY ym",
    "SELECT id FROM users ORDER BY FIELD(id, 3, 1, 2)",
    "SELECT * FROM users WHERE MATCH(name, email) AGAINST ('x' IN BOOLEAN MODE)",
    "UPDATE orders o, users u SET o.status = 'x' WHERE o.user_id = u.id AND u.name = 'z'",
    "DELETE FROM orders ORDER BY created_at LIMIT 10",
    "SELECT STRAIGHT_JOIN o.id FROM orders o JOIN users u ON u.id = o.user_id",
    "SELECT * FROM users u LEFT JOIN orders o ON o.user_id = u.id WHERE u.id IN (SELECT user_id FROM orders GROUP BY user_id HAVING COUNT(*) > 2) LOCK IN SHARE MODE",
    "SELECT name, JSON_UNQUOTE(JSON_EXTRACT(meta, '$.a')) FROM users",
    "SELECT * FROM users PARTITION (p0) WHERE id = 1",
    "SELECT CONCAT_WS(',', name, email), TIMESTAMPDIFF(DAY, created_at, NOW()) FROM users",
    "SELECT id FROM users LIMIT 5 OFFSET 10",
    "SELECT * FROM (SELECT user_id, ROW_NUMBER() OVER (PARTITION BY user_id ORDER BY total DESC) rn FROM orders) r WHERE r.rn = 1",
    "SELECT id, total FROM orders WHERE total > (SELECT AVG(total) FROM orders) INTO OUTFILE '/tmp/x'",
    "SELECT 1 FROM DUAL",
    "SELECT u.id FROM users u WHERE u.name REGEXP '^a' AND u.email NOT RLIKE 'z'",
];
const SL: &[&str] = &[
    "INSERT INTO users (id, name) VALUES (1,'a') ON CONFLICT(id) DO UPDATE SET name = excluded.name WHERE excluded.name <> users.name",
    "INSERT INTO users (id, name) VALUES (1,'a') ON CONFLICT DO NOTHING RETURNING id",
    "INSERT OR REPLACE INTO users (id, name) VALUES (1,'a')",
    "INSERT OR IGNORE INTO users (id) SELECT id FROM orders",
    "REPLACE INTO users (id) VALUES (1)",
    "WITH s AS (SELECT id FROM users) INSERT INTO orders (id, user_id) SELECT id, id FROM s",
    "WITH RECURSIVE c(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM c WHERE x<10) SELECT x FROM c",
    "CREATE TABLE g (a INT, b INT, c INT GENERATED ALWAYS AS (a+b) VIRTUAL, d INT AS (a*2) STORED); SELECT c, d FROM g",
    "SELECT j.key, j.value FROM users u, json_each(u.meta) AS j",
    "SELECT j.value ->> '$.a', u.meta -> 'b' FROM users u, json_tree(u.meta) j WHERE j.type = 'object'",
    "SELECT id, sum(total) OVER (ORDER BY id GROUPS BETWEEN 1 PRECEDING AND CURRENT ROW EXCLUDE NO OTHERS) FROM orders",
    "SELECT id FROM users WHERE name GLOB 'a*' AND email LIKE '%x' ESCAPE '\\'",
    "SELECT strftime('%Y-%m', created_at) AS ym, count(*) FROM orders GROUP BY ym",
    "SELECT * FROM users INDEXED BY idx_name WHERE name = 'a'",
    "SELECT * FROM users NOT INDEXED",
    "UPDATE orders SET total = u.id FROM users u WHERE u.id = orders.user_id",
    "DELETE FROM orders WHERE rowid IN (SELECT rowid FROM orders LIMIT 5)",
    "SELECT group_concat(name, ',') FROM users",
    "SELECT * FROM sqlite_master WHERE type='table'",
    "SELECT name FROM pragma_table_info('users')",
    "CREATE TABLE IF NOT EXISTS k (id INTEGER PRIMARY KEY AUTOINCREMENT, v TEXT) STRICT; INSERT INTO k (v) VALUES ('a'); SELECT v FROM k",
    "CREATE TEMP TABLE tt AS SELECT id FROM users; SELECT id FROM tt",
    "SELECT max(users.id), min(total) FROM orders, users",
    "SELECT typeof(id), iif(id>1,'a','b') FROM users",
    "SELECT * FROM users WHERE id IN (SELECT value FROM json_each('[1,2]'))",
    "SELECT x FROM (SELECT 1 AS x UNION SELECT 2) ORDER BY x",
    "BEGIN IMMEDIATE; UPDATE users SET name='a' WHERE id=1; COMMIT;",
    "SELECT main.users.id FROM main.users",
    "VACUUM; ANALYZE; REINDEX",
    "SELECT * FROM users WHERE name = :n AND id = @id AND email = ?1",
];
#[test]
fn pg() {
    let mut v: Vec<&str> = COMMON.to_vec();
    v.extend(PG);
    run(Dialect::Postgres, "public", &v);
}
#[test]
fn my() {
    let mut v: Vec<&str> = COMMON.to_vec();
    v.extend(MY);
    run(Dialect::MySql, "shop", &v);
}
#[test]
fn sl() {
    let mut v: Vec<&str> = COMMON.to_vec();
    v.extend(SL);
    run(Dialect::Sqlite, "main", &v);
}
#[test]
fn real_errors_are_still_reported() {
    for (dia, h) in [
        (Dialect::Postgres, "public"),
        (Dialect::MySql, "shop"),
        (Dialect::Sqlite, "main"),
    ] {
        for (q, code, at) in [
            (
                "SELECT nope FROM users",
                DiagnosticCode::UnknownColumn,
                "nope",
            ),
            (
                "SELECT id FROM nope",
                DiagnosticCode::UnknownRelation,
                "nope",
            ),
            (
                "WITH s AS (SELECT id FROM users) SELECT s.zz FROM s",
                DiagnosticCode::UnknownColumn,
                "zz",
            ),
            (
                "INSERT INTO orders (id, bad) VALUES (1,2)",
                DiagnosticCode::UnknownColumn,
                "bad",
            ),
            (
                "SELECT u.id FROM users u JOIN orders o ON o.uid = u.id",
                DiagnosticCode::UnknownColumn,
                "uid",
            ),
            (
                "SELECT id FROM users u JOIN orders o ON o.user_id=u.id",
                DiagnosticCode::AmbiguousColumn,
                "id",
            ),
            (
                "SELECT id FROM users UNION SELECT user_id FROM orders WHERE bogus = 1",
                DiagnosticCode::UnknownColumn,
                "bogus",
            ),
            (
                "INSERT INTO orders (id, user_id) WITH s AS (SELECT id FROM users) SELECT s.nope, 1 FROM s",
                DiagnosticCode::UnknownColumn,
                "nope",
            ),
            (
                "INSERT INTO orders (id) WITH s AS (SELECT id FROM users) SELECT 1 FROM nope",
                DiagnosticCode::UnknownRelation,
                "nope",
            ),
        ] {
            let got: Vec<_> = diagnose(q, dia, &schema(h))
                .iter()
                .map(|d| (d.code, q[d.range.clone()].to_owned()))
                .collect();
            assert_eq!(got, [(code, at.to_owned())], "{dia:?}: {q}");
        }
    }
}
