// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Realistic statements that must produce no diagnostics: stored routines (parameters and
//! `DECLARE`d locals are not columns), ORM-style quoted SQL, DDL migrations, bind parameters.

#![allow(clippy::needless_raw_string_hashes, clippy::unnecessary_wraps)]

use sqler_completion::{MemorySchema, diagnose};
use sqler_core::{ColumnInfo, DataType, ObjectKind};
use sqler_sql::Dialect;

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
    s.add_table(
        h,
        "auth_user",
        ObjectKind::Table,
        cols(&["id", "username", "is_active"]),
    );
    s.add_table(h, "unloaded", ObjectKind::Table, None);
    s
}
fn go(dia: Dialect, h: &str, l: &[&str]) {
    let mut bad = Vec::new();
    for q in l {
        let r = diagnose(q, dia, &schema(h));
        if !r.is_empty() {
            bad.push(format!(
                "{q}\n    -> {:?}",
                r.iter()
                    .map(|x| format!("{:?}@{}", x.code, &q[x.range.clone()]))
                    .collect::<Vec<_>>()
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "{dia:?} false positives:\n{}",
        bad.join("\n")
    );
}
const COMMON: &[&str] = &[
    r#"SELECT "auth_user"."id", "auth_user"."username" FROM "auth_user" WHERE "auth_user"."is_active" = true ORDER BY "auth_user"."id" ASC LIMIT 21"#,
    r#"SELECT COUNT(*) AS "__count" FROM "auth_user" WHERE "auth_user"."is_active""#,
    r#"SELECT "orders"."id", "orders"."user_id" FROM "orders" INNER JOIN "users" ON ("orders"."user_id" = "users"."id") WHERE "users"."name" LIKE 'a%' ESCAPE '\'"#,
    r#"SELECT "users"."id" FROM "users" WHERE "users"."id" IN (SELECT U0."user_id" FROM "orders" U0 WHERE U0."total" > 5)"#,
    r#"SELECT "users"."id", COUNT("orders"."id") AS "n" FROM "users" LEFT OUTER JOIN "orders" ON ("users"."id" = "orders"."user_id") GROUP BY "users"."id" HAVING COUNT("orders"."id") > 1"#,
    r#"SELECT "users"."id" FROM "users" WHERE EXISTS(SELECT (1) AS "a" FROM "orders" U0 WHERE U0."user_id" = ("users"."id") LIMIT 1)"#,
    r#"SELECT u.id FROM users u WHERE u.id > 10 ORDER BY u.id LIMIT 20 OFFSET 40"#,
    r#"SELECT id, name FROM users WHERE (created_at, id) < ('2024-01-01', 5) ORDER BY created_at DESC, id DESC FETCH FIRST 10 ROWS ONLY"#,
    r#"SELECT t0.id, t0.name FROM users AS t0 WHERE t0.id = (SELECT MAX(t1.user_id) FROM orders AS t1)"#,
    r#"SELECT count(*) FILTER (WHERE status = 'paid') AS paid, count(*) FILTER (WHERE status = 'x') AS x, user_id FROM orders GROUP BY user_id"#,
    r#"WITH RECURSIVE t(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM t WHERE n < 5) SELECT n FROM t"#,
    r#"WITH a AS (SELECT id, total FROM orders), b AS (SELECT id FROM a WHERE total > 1) SELECT b.id, a.total FROM b JOIN a ON a.id=b.id"#,
    r#"CREATE TABLE audit (id INTEGER PRIMARY KEY, ts TIMESTAMP NOT NULL, who TEXT); INSERT INTO audit (id, ts, who) VALUES (1, now(), 'x'); SELECT id, ts, who FROM audit WHERE who = 'x'; ALTER TABLE audit ADD COLUMN note TEXT; SELECT note FROM audit; DROP TABLE audit"#,
    r#"CREATE TABLE a2 (id INT, v INT); ALTER TABLE a2 RENAME COLUMN v TO w; SELECT w FROM a2"#,
    r#"CREATE INDEX idx_o ON orders (user_id, created_at DESC) WHERE status = 'open'"#,
    r#"CREATE UNIQUE INDEX idx_lower ON users (lower(email))"#,
    r#"CREATE VIEW paid AS SELECT id, total FROM orders WHERE status='paid'; SELECT p.id, p.total FROM paid p"#,
    r#"CREATE VIEW v2 (a, b) AS SELECT id, name FROM users; SELECT a, b FROM v2"#,
    r#"DROP VIEW IF EXISTS paid; DROP TABLE IF EXISTS zzz; DROP INDEX IF EXISTS foo"#,
    r#"SELECT * FROM unloaded u WHERE u.whatever = 1 AND zip > 2"#,
    r#"SELECT o.id, (SELECT count(*) FROM items i WHERE i.order_id = o.id) AS cnt FROM orders o ORDER BY cnt DESC"#,
    r#"SELECT user_id, SUM(total) AS s FROM orders GROUP BY user_id ORDER BY s DESC, user_id"#,
    r#"UPDATE orders SET status = 'done' WHERE id IN (SELECT order_id FROM items GROUP BY order_id HAVING sum(qty) > 3)"#,
    r#"SELECT CAST(id AS TEXT) AS sid, COALESCE(NULLIF(name,''), 'anon') FROM users WHERE name NOT IN ('a','b')"#,
    r#"SELECT 1 + 1 AS two, 'x' || 'y' AS xy"#,
    r#"SELECT u.id, o.* FROM users u JOIN orders o ON o.user_id = u.id WHERE o.total BETWEEN 1 AND 9 AND u.email IS NOT NULL"#,
    r#"SELECT a.id FROM users a JOIN users b ON a.id <> b.id WHERE a.name = b.name"#,
    r#"SELECT id FROM users WHERE id = :id AND name = ?"#,
    r#"BEGIN; UPDATE users SET name = 'x' WHERE id = 1; SAVEPOINT s1; DELETE FROM orders; ROLLBACK TO SAVEPOINT s1; COMMIT"#,
];
const PG: &[&str] = &[
    r#"SELECT u.id, t.tag FROM users u CROSS JOIN LATERAL unnest(string_to_array(u.name, ' ')) AS t(tag)"#,
    r#"SELECT u.id, t.tag, t.ord FROM users u, LATERAL unnest(ARRAY['a','b']) WITH ORDINALITY AS t(tag, ord)"#,
    r#"SELECT u.id, x FROM users u LEFT JOIN LATERAL unnest(ARRAY[1,2]) x ON true"#,
    r#"SELECT user_id, count(*) FILTER (WHERE status = 'paid') AS p, sum(total) FILTER (WHERE total > 0) FROM orders GROUP BY user_id"#,
    r#"SELECT percentile_cont(0.5) WITHIN GROUP (ORDER BY total) FROM orders"#,
    r#"CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_o2 ON orders USING btree (user_id) INCLUDE (total) WHERE status <> 'x'"#,
    r#"CREATE INDEX idx_gin ON users USING gin ((meta -> 'tags'))"#,
    r#"CREATE MATERIALIZED VIEW mv AS SELECT user_id, sum(total) AS s FROM orders GROUP BY user_id WITH DATA; REFRESH MATERIALIZED VIEW mv; SELECT user_id, s FROM mv"#,
    r#"CREATE OR REPLACE VIEW public.vv AS SELECT id FROM users; SELECT vv.id FROM public.vv"#,
    r#"CREATE OR REPLACE FUNCTION f(a int) RETURNS int AS $$ BEGIN RETURN a + bogus; END $$ LANGUAGE plpgsql; SELECT f(1)"#,
    r#"CREATE FUNCTION g() RETURNS trigger LANGUAGE plpgsql AS $body$ BEGIN NEW.updated_at := now(); RETURN NEW; END; $body$"#,
    r#"CREATE TRIGGER trg BEFORE UPDATE ON users FOR EACH ROW EXECUTE FUNCTION g()"#,
    r#"ALTER TABLE orders ADD CONSTRAINT fk FOREIGN KEY (user_id) REFERENCES users (id) ON DELETE CASCADE; ALTER TABLE orders ALTER COLUMN total SET NOT NULL; ALTER TABLE orders DROP COLUMN IF EXISTS junk"#,
    r#"ALTER TABLE users ADD COLUMN flags jsonb NOT NULL DEFAULT '{}'; SELECT flags FROM users"#,
    r#"SELECT "users"."id" FROM "users" WHERE "users"."id" = ANY(ARRAY[1,2,3]) AND "users"."meta" @> '{"a": 1}'::jsonb"#,
    r#"SELECT "orders"."id" FROM "orders" WHERE "orders"."created_at" >= '2024-01-01 00:00:00+00'::timestamptz FOR UPDATE OF "orders" NOWAIT"#,
    r#"INSERT INTO "users" ("name", "email") VALUES ('a', 'b') RETURNING "users"."id""#,
    r#"INSERT INTO users (id, name) VALUES (1, 'a'), (2, 'b') ON CONFLICT ON CONSTRAINT users_pkey DO NOTHING"#,
    r#"WITH moved AS (DELETE FROM orders WHERE status='old' RETURNING *) INSERT INTO items (order_id, sku) SELECT id, 'x' FROM moved"#,
    r#"SELECT * FROM (VALUES (1,'a'),(2,'b')) AS v(id, label) JOIN users u ON u.id = v.id"#,
    r#"SELECT jsonb_build_object('a', u.id) , jsonb_agg(o.total ORDER BY o.id) FROM users u JOIN orders o ON o.user_id=u.id GROUP BY u.id"#,
    r#"SELECT * FROM jsonb_to_recordset('[]'::jsonb) AS r(a int, b text) WHERE r.a > 1"#,
    r#"SELECT u.id FROM users u WHERE u.created_at > now() - interval '7 days' AND u.name ~* '^a'"#,
    r#"SELECT id, lead(id) OVER (PARTITION BY user_id ORDER BY created_at) AS nxt FROM orders WINDOW w AS (ORDER BY id)"#,
    r#"SELECT $1::int, $2 FROM users WHERE id = $1"#,
    r#"SELECT s.n FROM generate_series(1, 10) AS s(n) WHERE s.n % 2 = 0"#,
    r#"SELECT u.id FROM ONLY users u"#,
    r#"CREATE TYPE mood AS ENUM ('a','b'); CREATE SEQUENCE sq START 1; SELECT nextval('sq')"#,
    r#"CREATE TABLE ev (id bigint GENERATED ALWAYS AS IDENTITY, at timestamptz DEFAULT now(), CONSTRAINT ev_pk PRIMARY KEY (id), CHECK (at > '2000-01-01')) PARTITION BY RANGE (at); SELECT id, at FROM ev"#,
    r#"COMMENT ON COLUMN users.name IS 'x'; GRANT SELECT ON users TO PUBLIC; TRUNCATE users, orders RESTART IDENTITY CASCADE"#,
    r#"VACUUM ANALYZE users; ANALYZE orders; REINDEX TABLE users"#,
    r#"SELECT u.id FROM public.users u JOIN public.orders o ON o.user_id = u.id"#,
    r#"SELECT ( SELECT sum(i.qty) FROM items i WHERE i.order_id = o.id ) AS q FROM orders o"#,
];
const MY: &[&str] = &[
    r#"SELECT `users`.`id`, `users`.`name` FROM `users` WHERE `users`.`id` = 1 LIMIT 21"#,
    r#"SELECT `orders`.`id` FROM `orders` INNER JOIN `users` ON (`orders`.`user_id` = `users`.`id`) ORDER BY `orders`.`id` DESC LIMIT 10 OFFSET 20"#,
    r#"INSERT INTO `users` (`name`, `email`) VALUES ('a', 'b')"#,
    r#"UPDATE `users` SET `name` = 'x' WHERE `users`.`id` = 1"#,
    r#"CREATE VIEW paid AS SELECT id, total FROM orders; SELECT id FROM paid"#,
    r#"DELIMITER //
CREATE PROCEDURE p(IN uid INT) BEGIN DECLARE n INT DEFAULT 0; SELECT COUNT(*) INTO n FROM orders WHERE user_id = uid; SELECT n; END //
DELIMITER ;"#,
    r#"CREATE TABLE `t9` (`id` int NOT NULL AUTO_INCREMENT, `n` varchar(20) DEFAULT NULL, PRIMARY KEY (`id`), KEY `k` (`n`)) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4; SELECT `id`, `n` FROM `t9`"#,
    r#"ALTER TABLE users ADD COLUMN age INT AFTER name, ADD INDEX idx_age (age); SELECT age FROM users"#,
    r#"ALTER TABLE orders MODIFY COLUMN total DECIMAL(10,2) NOT NULL, DROP COLUMN status"#,
    r#"SELECT user_id, SUM(total) OVER (PARTITION BY user_id) FROM orders"#,
    r#"SELECT * FROM users u JOIN LATERAL (SELECT * FROM orders o WHERE o.user_id = u.id LIMIT 1) l ON TRUE"#,
    r#"SELECT JSON_UNQUOTE(JSON_EXTRACT(meta, '$.a')) AS a FROM users HAVING a IS NOT NULL"#,
    r#"INSERT INTO users (id, name) VALUES (1,'a') AS new ON DUPLICATE KEY UPDATE name = new.name"#,
    r#"SELECT * FROM JSON_TABLE('[]', '$[*]' COLUMNS (x INT PATH '$.x')) AS jt WHERE jt.x > 1"#,
    r#"REPLACE INTO users (id, name) VALUES (1,'a'); SELECT LAST_INSERT_ID(); SELECT FOUND_ROWS()"#,
    r#"SELECT DATE_FORMAT(created_at, '%Y-%m') AS ym, COUNT(*) FROM orders GROUP BY ym ORDER BY ym"#,
    r#"CREATE TRIGGER trg BEFORE INSERT ON users FOR EACH ROW SET NEW.created_at = NOW()"#,
    r#"SELECT * FROM users WHERE MATCH(name) AGAINST('x' IN BOOLEAN MODE)"#,
    r#"SET @a = 1; SET NAMES utf8mb4; SELECT @a"#,
    r#"SELECT u.id FROM shop.users u STRAIGHT_JOIN shop.orders o ON o.user_id=u.id"#,
    r#"CREATE INDEX idx_f ON users ((lower(email)))"#,
];
const SL: &[&str] = &[
    r#"SELECT "users"."id" FROM "users" WHERE "users"."id" = ?1 LIMIT 21"#,
    r#"SELECT id FROM users WHERE name = :name AND email = @email AND id = $id"#,
    r#"CREATE TABLE IF NOT EXISTS t (id INTEGER PRIMARY KEY AUTOINCREMENT, n TEXT NOT NULL UNIQUE, ts DATETIME DEFAULT CURRENT_TIMESTAMP) STRICT; SELECT id, n, ts FROM t"#,
    r#"CREATE TABLE w (a, b); INSERT INTO w VALUES (1,2); SELECT a, b, rowid FROM w"#,
    r#"CREATE INDEX i1 ON orders (user_id) WHERE status = 'open'"#,
    r#"CREATE INDEX i2 ON users (lower(email), created_at DESC)"#,
    r#"CREATE VIEW vw AS SELECT id, name FROM users; SELECT id, name FROM vw"#,
    r#"CREATE TRIGGER tr AFTER INSERT ON orders BEGIN UPDATE users SET name = NEW.id WHERE id = NEW.user_id; INSERT INTO items (order_id) VALUES (NEW.id); END"#,
    r#"CREATE VIRTUAL TABLE ft USING fts5(title, body); SELECT title FROM ft WHERE ft MATCH 'x'"#,
    r#"INSERT INTO users (id, name) VALUES (1, 'a') ON CONFLICT(id) DO UPDATE SET name = excluded.name"#,
    r#"INSERT OR REPLACE INTO users (id, name) VALUES (1, 'a')"#,
    r#"SELECT json_extract(meta, '$.a'), meta ->> '$.b', meta -> 'c' FROM users"#,
    r#"SELECT * FROM json_each('[1,2]') AS j WHERE j.value > 1"#,
    r#"PRAGMA foreign_keys = ON; PRAGMA table_info(users); SELECT sqlite_version()"#,
    r#"SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'"#,
    r#"ALTER TABLE users ADD COLUMN age INTEGER; ALTER TABLE users RENAME COLUMN age TO years; SELECT years FROM users"#,
    r#"SELECT u.id, group_concat(o.id, ',') FROM users u LEFT JOIN orders o ON o.user_id=u.id GROUP BY u.id"#,
    r#"SELECT id FROM users INDEXED BY idx WHERE id = 1"#,
    r#"SELECT id, row_number() OVER (ORDER BY id) FROM users WHERE name GLOB 'a*'"#,
    r#"WITH c AS MATERIALIZED (SELECT id FROM users) SELECT id FROM c"#,
    r#"SELECT main.users.id FROM main.users"#,
    r#"ATTACH DATABASE 'x.db' AS other; SELECT 1; DETACH DATABASE other"#,
    r#"SELECT strftime('%Y', created_at) AS y, count(*) FROM orders GROUP BY y"#,
    r#"BEGIN IMMEDIATE; DELETE FROM orders WHERE id = 1; COMMIT"#,
    r#"SELECT [id], [name] FROM [users]"#,
];

const ROUTINES_MY: &[&str] = &[
    "CREATE PROCEDURE p(IN uid INT) BEGIN SELECT COUNT(*) FROM orders WHERE user_id = uid; END",
    "CREATE PROCEDURE p(uid INT) SELECT * FROM orders WHERE user_id = uid",
    "CREATE FUNCTION f(uid INT) RETURNS INT DETERMINISTIC BEGIN DECLARE c INT; SELECT COUNT(*) INTO c FROM orders WHERE user_id = uid; RETURN c; END",
    "CREATE PROCEDURE p() BEGIN DECLARE n INT DEFAULT 0; SET n = n + 1; SELECT n; SELECT * FROM orders WHERE id = n; END",
    "CREATE PROCEDURE p(IN uid INT) BEGIN UPDATE orders SET total = 0 WHERE user_id = uid; END",
    "CREATE DEFINER = `root`@`localhost` PROCEDURE p(IN uid INT) BEGIN IF uid > 0 THEN SELECT * FROM orders WHERE user_id = uid; END IF; END",
    "CREATE EVENT ev ON SCHEDULE EVERY 1 DAY DO DELETE FROM orders WHERE created_at < cutoff",
];

#[test]
fn mysql_routines_are_silent() {
    go(Dialect::MySql, "shop", ROUTINES_MY);
}

#[test]
fn text_before_a_routine_is_still_checked() {
    let q = "SELECT nope FROM users; CREATE PROCEDURE p() BEGIN SELECT 1; END";
    assert_eq!(diagnose(q, Dialect::MySql, &schema("shop")).len(), 1);
}

#[test]
fn postgres_routines_are_silent() {
    go(
        Dialect::Postgres,
        "public",
        &[
            "CREATE FUNCTION f(uid int) RETURNS int LANGUAGE sql AS 'SELECT count(*)::int FROM orders WHERE user_id = uid'",
            "CREATE FUNCTION f(uid int) RETURNS int LANGUAGE sql BEGIN ATOMIC SELECT count(*)::int FROM orders WHERE user_id = uid; END",
            "CREATE PROCEDURE p(uid int) LANGUAGE sql BEGIN ATOMIC UPDATE orders SET total = 0 WHERE user_id = uid; END",
        ],
    );
}

#[test]
fn postgres_corpus_is_silent() {
    let mut all: Vec<&str> = COMMON.to_vec();
    all.extend_from_slice(PG);
    go(Dialect::Postgres, "public", &all);
}

#[test]
fn mysql_corpus_is_silent() {
    // `FROM "orders" U0` (double-quoted relation) is not valid in default MySQL mode; skipped.
    let mut all: Vec<&str> = COMMON
        .iter()
        .copied()
        .filter(|q| !q.contains("\"auth_user\"") && !q.contains("U0"))
        .collect();
    all.retain(|q| !q.contains("\"users\"") && !q.contains("\"orders\""));
    all.extend_from_slice(MY);
    go(Dialect::MySql, "shop", &all);
}

#[test]
fn sqlite_corpus_is_silent() {
    let mut all: Vec<&str> = COMMON.to_vec();
    all.extend_from_slice(SL);
    go(Dialect::Sqlite, "main", &all);
}
