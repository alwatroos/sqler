// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Case tables for statement classification.

use StatementClass::{Ddl, Other, Read, Unknown, Write};
use sqler_sql::{Dialect, StatementClass, classify, classify_each};

fn check(dialect: Dialect, cases: &[(&str, StatementClass)]) {
    for (sql, want) in cases {
        assert_eq!(classify(dialect, sql), *want, "{dialect:?}: {sql}");
    }
}

#[test]
fn lead_words_every_dialect() {
    let leads: &[(&str, StatementClass)] = &[
        ("select 1", Read),
        ("SELECT * from t", Read),
        ("(select 1)", Read),
        ("((select 1)) union (select 2)", Read),
        ("with a as (select 1) select * from a", Read),
        ("values (1, 2)", Read),
        ("table t", Read),
        ("show tables", Read),
        ("explain select 1", Read),
        ("describe t", Read),
        ("desc t", Read),
        ("insert into t values (1)", Write),
        ("update t set a = 1", Write),
        ("delete from t", Write),
        (
            "merge into t using s on true when matched then delete",
            Write,
        ),
        ("replace into t values (1)", Write),
        ("upsert into t values (1)", Write),
        ("copy t from stdin", Write),
        ("load data infile 'x' into table t", Write),
        ("refresh materialized view v", Write),
        ("create table t (a int)", Ddl),
        ("alter table t add b int", Ddl),
        ("drop table t", Ddl),
        ("truncate t", Ddl),
        ("rename table a to b", Ddl),
        ("comment on table t is 'x'", Ddl),
        ("grant select on t to u", Ddl),
        ("revoke select on t from u", Ddl),
        ("security label for p on table t is 'x'", Ddl),
        ("import foreign schema s from server x into y", Ddl),
        ("reindex table t", Ddl),
        ("begin", Other),
        ("start transaction", Other),
        ("commit", Other),
        ("end", Other),
        ("rollback", Other),
        ("savepoint a", Other),
        ("release a", Other),
        ("set x = 1", Other),
        ("reset all", Other),
        ("use db", Other),
        ("prepare p as select 1", Other),
        ("deallocate p", Other),
        ("discard all", Other),
        ("listen c", Other),
        ("unlisten c", Other),
        ("notify c", Other),
        ("checkpoint", Other),
        ("vacuum", Other),
        ("analyze t", Other),
        ("cluster t", Other),
        ("lock table t", Other),
        ("unlock tables", Other),
        ("flush tables", Other),
        ("kill 1", Other),
        ("attach 'f' as x", Other),
        ("detach x", Other),
        ("optimize table t", Other),
        ("repair table t", Other),
        ("handler t open", Other),
        ("call p()", Unknown),
        ("do 1", Unknown),
        ("exec p", Unknown),
        ("execute p", Unknown),
        ("frobnicate", Unknown),
        ("", Unknown),
        ("   ;  ", Unknown),
        ("-- only a comment", Unknown),
        ("'str'", Unknown),
        ("1", Unknown),
        ("\\d", Unknown),
    ];
    for d in Dialect::ALL {
        check(d, leads);
    }
}

#[test]
fn sqlite_pragmas() {
    check(
        Dialect::Sqlite,
        &[
            ("pragma table_info(t)", Read),
            ("PRAGMA main.table_info('t')", Read),
            ("pragma table_xinfo(t)", Read),
            ("pragma table_list", Read),
            ("pragma index_list(t)", Read),
            ("pragma index_info(i)", Read),
            ("pragma index_xinfo(i)", Read),
            ("pragma foreign_key_list(t)", Read),
            ("pragma database_list", Read),
            ("pragma collation_list", Read),
            ("pragma function_list", Read),
            ("pragma module_list", Read),
            ("pragma pragma_list", Read),
            ("pragma compile_options", Read),
            ("pragma table_info = 1", Other),
            ("pragma table_info(t) = 1", Other),
            ("pragma table_info(t) x", Other),
            ("pragma table_info(t", Other),
            ("pragma table_info('t", Unknown),
            ("pragma user_version", Other),
            ("pragma user_version = 3", Other),
            ("pragma foreign_keys = on", Other),
            ("pragma writable_schema = 1", Other),
            ("pragma", Other),
        ],
    );
    for d in [Dialect::Postgres, Dialect::MySql] {
        check(
            d,
            &[
                ("pragma table_info(t)", Unknown),
                ("pragma user_version", Unknown),
            ],
        );
    }
}

#[test]
fn violations() {
    for d in Dialect::ALL {
        check(
            d,
            &[
                (
                    "with d as (delete from t returning *) select * from d",
                    Write,
                ),
                (
                    "with d as (insert into t values (1) returning *) select 1",
                    Write,
                ),
                ("with d as (update t set a = 1 returning *) select 1", Write),
                ("select * into t2 from t", Write),
                ("select * from t into outfile '/tmp/x'", Write),
                ("select * from t into dumpfile '/tmp/x'", Write),
                ("select a into @v from t", Write),
                ("select * from t for update", Write),
                ("select * from t for share", Write),
                ("select * from t for no key update", Write),
                ("select * from t lock in share mode", Write),
                ("explain analyze delete from t", Write),
                ("explain (analyze) update t set a = 1", Write),
                ("explain analyze select 1", Read),
                ("select 1; drop table t", Ddl),
                ("select 1; insert into t values (1)", Write),
                ("select 1; select 2", Read),
                ("select 1; call p()", Unknown),
                ("select 1; frob", Unknown),
                ("select 1; set x = 1", Other),
                ("values (nextval('s'))", Unknown),
                ("select \"count\"(*)", Unknown),
                ("select s.f(1)", Unknown),
                ("select pg_catalog.count(*)", Unknown),
                ("-- drop\nselect 1", Read),
                ("select 1 /* drop table t */", Read),
                ("select 'drop table t; delete from x'", Read),
                ("select 1 from t where a = 'x' -- delete", Read),
                ("select 'unterminated", Unknown),
                ("select 1 /* unterminated", Unknown),
                ("select \"unterminated", Unknown),
                ("select @a := 1", Unknown),
                ("select 1 where a := 1", Unknown),
                ("select 1 do", Unknown),
                ("select 1 call", Unknown),
                ("select 1 exec", Unknown),
                ("select 1 execute", Unknown),
                ("select create_date from t", Read),
                (
                    "select 1 from t where a in (1, 2) and exists (select 1)",
                    Read,
                ),
                (
                    "select count(*) over (partition by a order by b) from t",
                    Read,
                ),
                (
                    "select cast(a as varchar(10)), cast(b as numeric(10, 2)) from t",
                    Read,
                ),
                ("select replace(a, 'x', 'y') from t", Read),
                ("select a from t where b = (select max(c) from u)", Read),
                ("select a from t x (y)", Unknown),
                ("with x(a) as (select 1) select a from x", Unknown),
                ("select count (*) from t", Read),
                ("select COUNT(*) from t", Read),
                ("select a from t limit 1 offset 2", Read),
                ("select a from t order by 1", Read),
                ("select case when a then (1) else (2) end from t", Read),
            ],
        );
    }
}

#[test]
fn replace_word_is_a_write_unless_called() {
    for d in Dialect::ALL {
        check(
            d,
            &[
                ("select 1 replace", Write),
                ("select replace(a,'b','c')", Read),
                ("select a from t replace into", Write),
            ],
        );
    }
}

#[test]
fn mysql_executable_comments() {
    check(
        Dialect::MySql,
        &[
            ("/*!50000 drop table x*/", Unknown),
            ("select /*!50000 1 */ 1", Unknown),
            ("select /*!40101 sleep(1) */ 1", Unknown),
            ("select /*M!100100 1 */ 1", Unknown),
            ("select 1 /* plain */", Read),
            ("select 1 -- x\n", Read),
            ("select 1 # x", Read),
            ("select @@version", Read),
            ("select @a", Read),
            ("select 1 into @v", Write),
        ],
    );
    // On the other dialects `/*!` is a plain comment.
    check(Dialect::Postgres, &[("select /*! 1 */ 1", Read)]);
}

#[test]
fn dangerous_functions_are_never_read() {
    let funcs = [
        "nextval",
        "setval",
        "set_config",
        "pg_sleep",
        "sleep",
        "benchmark",
        "get_lock",
        "pg_advisory_lock",
        "pg_terminate_backend",
        "pg_cancel_backend",
        "pg_read_file",
        "lo_import",
        "lo_export",
        "dblink",
        "dblink_exec",
        "query_to_xml",
        "txid_current",
        "pg_notify",
        "load_file",
        "load_extension",
        "last_insert_id",
        "currval",
        "lastval",
        "pg_reload_conf",
        "pg_read_binary_file",
        "pg_ls_dir",
        "pg_stat_file",
        "pg_advisory_xact_lock",
        "release_lock",
        "is_free_lock",
        "random_bytes",
        "randomblob",
        "writefile",
        "readfile",
        "zeroblob",
        "pragma_table_info",
        "json_table",
        "xmlparse",
        "pg_switch_wal",
        "pg_create_restore_point",
        "lo_unlink",
        "lo_creat",
        "lowrite",
        "current_setting",
        "pg_logical_emit_message",
    ];
    for d in Dialect::ALL {
        for f in funcs {
            for sql in [
                format!("select {f}(1)"),
                format!("SELECT {}('x')", f.to_uppercase()),
                format!("select 1 from t where {f} ( 1 )"),
                format!("values ({f}(1))"),
                format!("with x as (select {f}(1)) select * from x"),
                format!("select * from {f}(1)"),
                format!("explain select {f}(1)"),
                format!("select a.{f}(1)"),
                format!("select \"{f}\"(1)"),
            ] {
                assert!(!classify(d, &sql).is_read_only(), "{d:?}: {sql}");
            }
        }
    }
}

#[test]
fn allowlisted_functions_are_case_insensitive_and_unqualified_only() {
    for d in Dialect::ALL {
        check(
            d,
            &[
                ("select NOW(), Lower(a), COALESCE(a, b) from t", Read),
                ("select pg_catalog.now()", Unknown),
                ("select \"now\"()", Unknown),
                ("select nowx()", Unknown),
                ("select no()", Unknown),
            ],
        );
    }
}

#[test]
fn compound_begin_is_unknown_on_mysql() {
    check(
        Dialect::MySql,
        &[
            ("begin not atomic delete from t; end", Unknown),
            ("begin", Other),
            (
                "create trigger t before insert on x for each row set new.a = 1",
                Ddl,
            ),
        ],
    );
}

#[test]
fn multi_statement_text_classifies_per_statement() {
    let text = "select 1; update t set a = 1; create table x (a int); begin; frob";
    let each = classify_each(Dialect::Postgres, text);
    let classes: Vec<_> = each.iter().map(|(_, c)| *c).collect();
    assert_eq!(classes, vec![Read, Write, Ddl, Other, Unknown]);
    assert_eq!(classify(Dialect::Postgres, text), Unknown);
    assert_eq!(classify(Dialect::Postgres, "select 1;;select 2;"), Read);
    assert_eq!(&text[each[1].0.body.clone()], "update t set a = 1");
}

#[test]
fn mysql_delimiter_scripts() {
    let text = "DELIMITER //\nselect 1//\ndrop table t//\nDELIMITER ;\nselect 2;";
    let classes: Vec<_> = classify_each(Dialect::MySql, text)
        .into_iter()
        .map(|(_, c)| c)
        .collect();
    assert_eq!(classes, vec![Read, Ddl, Read]);
}

#[test]
fn class_helpers() {
    assert!(Read.is_read_only());
    for c in [Other, Write, Ddl, Unknown] {
        assert!(!c.is_read_only());
    }
    assert!(Read < Other && Other < Write && Write < Ddl && Ddl < Unknown);
    let names: Vec<_> = [Read, Other, Write, Ddl, Unknown]
        .iter()
        .map(|c| c.as_str())
        .collect();
    assert_eq!(names, ["read", "other", "write", "ddl", "unknown"]);
}

#[test]
fn stray_characters_are_not_read() {
    for d in Dialect::ALL {
        check(d, &[("select 1 \\g", Unknown), ("select 1 \0", Unknown)]);
    }
}

#[test]
fn postgres_casts() {
    check(
        Dialect::Postgres,
        &[("select b::numeric(10, 2), c::int[] from t", Read)],
    );
}

#[test]
fn unterminated_mysql_quoted_variables_are_not_read() {
    check(
        Dialect::MySql,
        &[
            ("select @'a", Unknown),
            ("select @\"x", Unknown),
            ("select @`x", Unknown),
            ("select @`x``", Unknown),
            ("select @'a'", Read),
            ("select @`x`", Read),
        ],
    );
}

fn mysql_and_others(sql: &str, mysql: StatementClass, others: StatementClass) {
    assert_eq!(classify(Dialect::MySql, sql), mysql, "MySql: {sql}");
    for d in [Dialect::Postgres, Dialect::Sqlite] {
        assert_eq!(classify(d, sql), others, "{d:?}: {sql}");
    }
}

/// R1: servers treat non-ASCII whitespace and control chars as identifier characters.
#[test]
fn non_ascii_whitespace_is_not_a_separator() {
    for c in ['\u{a0}', '\u{85}', '\u{3000}', '\u{FEFF}'] {
        for d in Dialect::ALL {
            for sql in [
                format!("select{c}1"),
                format!("select 1{c}"),
                format!("select{c}E'\\'; drop table t; --'"),
            ] {
                assert_ne!(classify(d, &sql), Read, "{d:?}: {sql:?}");
            }
        }
    }
    let sql = "SELECT 1 --\u{3000}x FROM (SELECT 1 AS \u{3000}x) t; DROP TABLE t2";
    // `--` is a comment to the end of the line on PostgreSQL and SQLite, whatever follows it.
    assert_eq!(classify_each(Dialect::MySql, sql).len(), 2);
    assert_ne!(classify(Dialect::MySql, sql), Read);
    for c in ['\u{a0}', '\u{85}', '\u{3000}', '\u{FEFF}'] {
        let sql = format!("select 1 --{c}x into outfile '/tmp/x'");
        assert_ne!(classify(Dialect::MySql, &sql), Read, "{sql:?}");
    }
    let sql = "SELECT 1 --\u{a0}x INTO OUTFILE '/tmp/x'";
    assert_ne!(classify(Dialect::MySql, sql), Read);
}

/// R2: MariaDB sequences.
#[test]
fn mariadb_sequence_forms_are_not_read() {
    let my = Dialect::MySql;
    check(
        my,
        &[
            ("select next value for s", Write),
            ("SELECT NEXT   VALUE FOR db.s", Write),
            ("select 1, next /* c */ value for s", Write),
            ("select previous value for s", Unknown),
            ("select nextval(s)", Unknown),
            ("select lastval()", Unknown),
            ("select setval(s, 1)", Unknown),
            ("select 1 as next, 2 as value", Read),
        ],
    );
    check(Dialect::Postgres, &[("select lastval()", Unknown)]);
}

/// R3: structural words that are valid function names are calls outside their position.
#[test]
fn structural_words_as_function_names_are_calls() {
    for sql in [
        "select filter(1)",
        "select partition(1)",
        "select explain(1)",
        "select 1 where true and by(1) = 1",
        "select over(1)",
        "select materialized(1)",
        "select recursive(1)",
        "select a, like(1)",
        "select 1 where x = join(1)",
        "select 1 where x is (select 1) and ilike(2)",
    ] {
        for d in Dialect::ALL {
            assert_eq!(classify(d, sql), Unknown, "{d:?}: {sql}");
        }
    }
    check(
        Dialect::Postgres,
        &[
            (
                "select count(*) filter (where a > 1) over (partition by b order by c) from t",
                Read,
            ),
            ("explain (analyze) select 1", Read),
            ("select * from a join (select 1) b on true", Read),
            ("select a from t group by (a) order by (a)", Read),
            ("select a like ('x'), b is (null is null) from t", Read),
            (
                "with recursive r as materialized (select 1) select * from r",
                Read,
            ),
        ],
    );
}

/// R4: mode-dependent string lexing fails safe.
#[test]
fn mode_dependent_strings_fail_safe() {
    check(
        Dialect::MySql,
        &[("select '\\'; drop table t; -- '", Unknown)],
    );
    check(Dialect::Sqlite, &[("select '\\'; drop table t; -- '", Ddl)]);
    check(
        Dialect::Postgres,
        &[("select '\\'; drop table t; -- '", Unknown)],
    );
    check(Dialect::MySql, &[("select 'a\\b'", Unknown)]);
    check(Dialect::Postgres, &[("select 'a\\b'", Unknown)]);
    mysql_and_others("select \"a\"", Unknown, Read);
    check(
        Dialect::MySql,
        &[("select \"\\\"; drop table t; -- \"", Unknown)],
    );
    check(Dialect::MySql, &[("select @\"a\"", Unknown)]);
    mysql_and_others("select count(*) from \"delete\"", Unknown, Read);
    mysql_and_others("select 'abc'", Read, Read);
    mysql_and_others("select X'41', B'1'", Read, Read);
    check(
        Dialect::Postgres,
        &[
            ("select '\\'' ; drop table t; --'", Unknown),
            ("select E'a\\nb'", Read),
            ("select U&'\\0041'", Unknown),
        ],
    );
    check(Dialect::Sqlite, &[("select 'a\\b'", Read)]);
}

/// R5: SQLite brackets do not double.
#[test]
fn sqlite_bracket_identifiers() {
    let sql = "select [a]] ; drop table t; --]";
    assert_eq!(classify_each(Dialect::Sqlite, sql).len(), 2);
    assert_ne!(classify(Dialect::Sqlite, sql), Read);
    check(Dialect::Sqlite, &[("select [a b] from [t]", Read)]);
}

/// R6: text after `DELIMITER x` is never silently skipped.
#[test]
fn delimiter_line_tail_is_classified() {
    let my = Dialect::MySql;
    let sql = "SELECT 1;\nDELIMITER ; DROP TABLE t";
    assert_ne!(classify(my, sql), Read);
    let sql = "DELIMITER // DROP TABLE t\n//";
    assert_ne!(classify(my, sql), Read);
    check(
        my,
        &[
            ("select 1;\nDELIMITER //\nselect 2//", Read),
            ("DELIMITER // -- note\nselect 2//", Read),
            ("DELIMITER // # note\nselect 2//", Read),
        ],
    );
}
