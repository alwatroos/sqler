// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Snapshots of formatted SQL per dialect. Every case is also checked for idempotency.

use sqler_sql::{Dialect, FormatError, FormatOptions, format};

fn run(dialect: Dialect, sql: &str) -> String {
    let options = FormatOptions::default();
    let once = format(dialect, sql, &options).unwrap_or_else(|e| panic!("{e}"));
    let twice = format(dialect, &once, &options).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(once, twice, "not idempotent for {dialect:?}");
    once
}

/// Snapshots `sql` for every dialect.
fn all(name: &str, sql: &str) {
    for dialect in Dialect::ALL {
        let out = run(dialect, sql);
        insta::assert_snapshot!(format!("{name}__{}", dialect.as_str()), out);
    }
}

#[test]
fn select_with_joins_subquery_and_cte() {
    all(
        "select_joins",
        "with recent as (select id, total from orders where created > '2024-01-01'),\n\
         big as (select * from recent where total between 10 and 20 and total <> 15)\n\
         select o.id, c.name, count(*) as n from orders o inner join customers c on c.id = o.customer_id \
         left outer join big b on b.id = o.id cross join t \
         where o.id in (select id from recent where total > 5) and (c.active or c.vip) \
         group by o.id, c.name having count(*) > 1 order by n desc limit 10 union all select 1, 'x', 2",
    );
}

#[test]
fn insert_update_delete() {
    all(
        "dml",
        "insert into t (a, b) values (1, 'it''s'), (2, 'x;y'); update t set a=a+1,b = 'z' where id=3 and name like 'a%';\n\
         delete from t where id in (select id from u where u.x is not null);",
    );
}

#[test]
fn comments_are_kept() {
    all(
        "comments",
        "-- leading\nselect /* inline */ a, -- trailing\n b from t /* multi\n   line */ where x = 1 -- end\n;\n\n-- only a comment\n/* another */",
    );
}

#[test]
fn qualified_names_keep_their_case() {
    all(
        "qualified",
        "SELECT t.order, t.Select, db.from.x FROM db.t WHERE t.limit = 1 order by t.order",
    );
}

#[test]
fn postgres_dollar_quoted_function() {
    let sql = "create function f(a int) returns int language plpgsql as $fn$\nbegin\n  if a > 1 then return a; end if;\n  return 0;\nend;\n$fn$;\nselect   f(1)   ;select $$ a;  b $$";
    insta::assert_snapshot!("pg_dollar_function", run(Dialect::Postgres, sql));
}

#[test]
fn postgres_specifics() {
    let sql = "select a::int, b[1:2], c->>'k', $1, :name from t where x ? 'k' and y @> '{}'::jsonb and z between 1 and 2 or w = E'a\\'b'";
    insta::assert_snapshot!("pg_specifics", run(Dialect::Postgres, sql));
}

#[test]
fn mysql_delimiter_procedure() {
    let sql = "delimiter //\ncreate procedure p(in x int)\nbegin\n  declare y int;\n  if x > 1 then set y = 1; end if;\n  select   y;\nend//\ndelimiter ;\nselect  1  ,2 ; # comment\nselect `a`  from `b` where c = \"s;t\"";
    insta::assert_snapshot!("mysql_delimiter", run(Dialect::MySql, sql));
}

#[test]
fn sqlite_trigger() {
    let sql = "create trigger trg after insert on t begin\n  update u set n = n + 1 where id = new.id;\n  insert into log values (case when new.a > 1 then 'big' else 'small' end);\nend;\nselect [a b], \"c\" from t";
    insta::assert_snapshot!("sqlite_trigger", run(Dialect::Sqlite, sql));
}

#[test]
fn options_are_honoured() {
    let mut options = FormatOptions::default();
    options.indent_width = 4;
    options.uppercase_keywords = false;
    let out = format(
        Dialect::Postgres,
        "select a from t where (select 1) = 1 and b = 2",
        &options,
    )
    .expect("formats");
    insta::assert_snapshot!("options_four_no_upper", out);
}

#[test]
fn blank_and_unterminated() {
    for dialect in Dialect::ALL {
        let options = FormatOptions::default();
        assert_eq!(format(dialect, "", &options), Ok(String::new()));
        assert_eq!(format(dialect, " \n\t ", &options), Ok(String::new()));
        assert_eq!(
            format(dialect, "select 1;\nselect 'abc", &options),
            Err(FormatError::Unterminated { offset: 17 })
        );
        assert_eq!(
            format(dialect, "select /* x", &options),
            Err(FormatError::Unterminated { offset: 7 })
        );
        assert!(matches!(
            format(dialect, "select \"x", &options),
            Err(FormatError::Unterminated { .. })
        ));
    }
}
