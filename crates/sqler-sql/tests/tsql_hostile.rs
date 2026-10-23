// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The T-SQL hostile table: statements whose class must be exactly what is listed. `Read` is the
//! only class that runs without asking, so a `Read` that is not provably read-only is a safety bug.

use StatementClass::{Ddl, Other, Read, Unknown, Write};
use sqler_sql::{Dialect, SegmentKind, StatementClass, classify, split};

const T: Dialect = Dialect::TSql;

fn check(want: StatementClass, cases: &[&str]) {
    for sql in cases {
        assert_eq!(classify(T, sql), want, "{sql:?}");
    }
}

fn statements(sql: &str) -> Vec<&str> {
    split(T, sql)
        .into_iter()
        .filter(|s| s.kind == SegmentKind::Statement)
        .map(|s| &sql[s.body])
        .collect()
}

#[test]
fn read() {
    check(
        Read,
        &[
            "select 1 -- ; delete from t",
            "select '; GO\ndelete from t'",
            "select [a]] ; drop table t; --]",
            "select 1 /* /* */ ; drop table t; */",
            "select N'x''; GO'",
            "select top (5) * from t with (nolock) option (recompile)",
            "select @@rowcount, @x, $5, 0x0A from #t",
            "select getdate(), isnull(a,0) from [dbo].[t]",
        ],
    );
}

#[test]
fn write() {
    check(
        Write,
        &[
            "select 1\nGO\ndelete from t",
            "select 1\nGO 2\ndelete from t",
            "select * from t with (updlock)",
            "with c as (select 1 a) delete from c",
            "update t set a=1 output inserted.* into log",
            "select next value for s",
            "select a into #t from b",
            "waitfor (receive top(1) * from q)",
            "dbcc checkdb",
            "backup database d to disk='x'",
        ],
    );
}

/// SQL Server ends a numeric literal at the first character that cannot continue it and needs no
/// separator before the next statement, so a keyword glued to a number still runs.
#[test]
fn glued_number_keywords_are_seen() {
    check(
        Write,
        &[
            "select 1delete t",
            "select 1e5delete t",
            "select 1.5delete t",
            "select $1delete t",
            "select 1e delete t",
            "select 1.delete t",
            "select 1e+5delete t",
            "select 0x1F delete t",
            "select 1delete t; select 2",
            "get conversation group @g from q",
            "select 1 get conversation group @g from q",
            "GET CONVERSATION GROUP @g FROM q",
        ],
    );
    // Glued to a word the classifier does not understand: never `Read`.
    check(
        Unknown,
        &[
            "select 1from t",
            "select 1as a",
            "select 0x1Fdelete t",
            "select 1e5x",
            "select $1x",
            "select 1_a",
        ],
    );
    check(
        Read,
        &["select 1e5, 1.5, 0x1F, $1.5, 1 from t", "select 1e+5 as a"],
    );
}

#[test]
fn unknown() {
    check(
        Unknown,
        &[
            "exec sp_who",
            "EXECUTE('drop table t')",
            "exec sp_executesql N'delete from t'",
            "insert into t exec p",
            "sp_who",
            "[dbo].[p]",
            "select dbo.f(1)",
            "select * from openquery(s,'delete t')",
            "select geography::Point(1,2,4326)",
            "begin delete from t end",
            "begin try select 1 end try begin catch end catch",
            "if 1=1 delete from t",
            "select [unterminated",
            "select `x`",
        ],
    );
}

#[test]
fn ddl() {
    check(
        Ddl,
        &[
            "create procedure p as select 1; delete from t",
            "deny select on t to u",
            "truncate table t",
        ],
    );
    // The routine body is one statement: nothing after the `;` is split off.
    assert_eq!(
        statements("create procedure p as select 1; delete from t"),
        ["create procedure p as select 1; delete from t"]
    );
}

#[test]
fn other() {
    check(
        Other,
        &[
            "set nocount on",
            "use master",
            "begin tran",
            "declare @x int = 1",
            "waitfor delay '00:00:01'",
            "save tran s",
            "commit tran",
            "begin",
        ],
    );
}

/// T-SQL separates statements by whitespace, so any statement can follow a read with no `;`.
#[test]
fn chained_statements_are_never_read() {
    for tail in [
        "dbcc freeproccache",
        "backup log d to disk = 'x'",
        "restore database d from disk = 'x'",
        "bulk insert t from 'f'",
        "reconfigure",
        "shutdown",
        "kill 52",
        "checkpoint",
        "send on conversation @h ('x')",
        "end conversation @h",
        "move conversation @h to @g",
        "waitfor delay '00:00:10'",
        "updatetext t.c @p 0 0 'x'",
        "writetext t.c @p 'x'",
        "deny select on t to u",
        "enable trigger tr on t",
        "disable trigger tr on t",
        "add signature to p by certificate c",
        "setuser 'x'",
        "revert",
        "open symmetric key k decryption by certificate c",
        "set implicit_transactions on",
        "use master",
        "begin tran",
        "commit",
        "rollback",
        "declare @x int",
        "print 'x'",
        "save tran s",
        "exec p",
        "execute as user = 'x'",
        "truncate table t",
        "drop table t",
        "delete from t",
        "grant select on t to u",
        "merge t using s on 1=1 when matched then delete;",
    ] {
        for glue in [" ", "\n", " /* c */ ", "\r\n", "\nGO 2\n", "; "] {
            let text = format!("select 1{glue}{tail}");
            assert_ne!(classify(T, &text), Read, "{text:?}");
        }
    }
}

#[test]
fn go_is_a_separator_only_alone_on_its_line() {
    assert_eq!(
        statements("select 1\nGO\nselect 2"),
        ["select 1", "select 2"]
    );
    assert_eq!(
        statements("select 1\r\ngo\r\nselect 2"),
        ["select 1", "select 2"]
    );
    // A comment after `GO` belongs to the next batch.
    assert_eq!(
        statements("select 1\n  \tGo \t -- c\nselect 2"),
        ["select 1", "-- c\nselect 2"]
    );
    assert_eq!(statements("GO\nselect 1\nGO"), ["select 1"]);
    // Not separators: the text stays one statement and every word of it is scanned.
    // `GO;` is ended by the `;`, with `GO` left in the first statement.
    assert_eq!(
        statements("select 1\nGO;\nselect 2"),
        ["select 1\nGO", "select 2"]
    );
    for text in [
        "select 1\nGO 2\nselect 2",
        "select 1 GO\nselect 2",
        "select 1\n/* x */ GO\nselect 2",
        "select 1\nGO /* x */\nselect 2",
        "select '\nGO\n'",
        "select /*\nGO\n*/ 1",
        "select [\nGO\n]",
        "select 1 -- x\n-- GO\nselect 2",
    ] {
        assert_eq!(statements(text).len(), 1, "{text:?}");
    }
}

#[test]
fn go_ends_every_state() {
    // Inside a block, a routine and a string-free comment tail.
    assert_eq!(
        statements("begin try\nselect 1\nGO\nselect 2"),
        ["begin try\nselect 1", "select 2"]
    );
    assert_eq!(
        statements("create proc p as\nbegin\nselect 1;\nGO\nselect 2;"),
        ["create proc p as\nbegin\nselect 1;", "select 2"]
    );
    assert_eq!(
        statements("alter function f() returns int as begin return 1; end\nGO\ndelete from t"),
        [
            "alter function f() returns int as begin return 1; end",
            "delete from t"
        ]
    );
    assert_eq!(
        classify(T, "begin try\nselect 1\nGO\ndelete from t"),
        Unknown
    );
    assert_eq!(classify(T, "select 1\nGO\nsp_who"), Unknown);
    assert_eq!(classify(T, "select 1\nGO\nexec p"), Unknown);
}

#[test]
fn blocks_and_routines() {
    // `BEGIN TRY .. END TRY BEGIN CATCH .. END CATCH` is one statement.
    let text = "begin try select 1; delete from t; end try begin catch select 2; end catch";
    assert_eq!(statements(text), [text]);
    // A routine is ended only by `GO` or the end of the text.
    for text in [
        "create procedure p as select 1; delete from t",
        "CREATE OR ALTER PROC p AS select 1; delete from t",
        "alter trigger tr on t after insert as delete from u; delete from v",
        "create function f() returns int as begin return 1; end; select 2",
    ] {
        assert_eq!(statements(text), [text], "{text:?}");
    }
    // Transaction control does not open a block.
    assert_eq!(
        statements("begin tran; select 1; commit tran"),
        ["begin tran", "select 1", "commit tran"]
    );
    assert_eq!(statements("begin; select 1"), ["begin", "select 1"]);
    assert_eq!(
        statements("begin distributed transaction; select 1"),
        ["begin distributed transaction", "select 1"]
    );
    assert_eq!(
        statements("begin transaction t1; select 1; commit"),
        ["begin transaction t1", "select 1", "commit"]
    );
    // `CASE .. END` is balanced; `;` after it splits again.
    assert_eq!(
        statements("select case when 1=1 then 2 end; select 3"),
        ["select case when 1=1 then 2 end", "select 3"]
    );
    // `END CONVERSATION` closes nothing.
    assert_eq!(
        statements("end conversation @h; select 1"),
        ["end conversation @h", "select 1"]
    );
    // An unclosed block swallows the rest, which is the safe side.
    let text = "begin select 1; delete from t";
    assert_eq!(statements(text), [text]);
    assert_eq!(classify(T, text), Unknown);
    let text = "select 1 begin; delete from t";
    assert_eq!(classify(T, text), Write);
}

#[test]
fn string_and_identifier_forms_do_not_hide_words() {
    // Each is one token, so the `;` and `delete` inside cannot split or write.
    check(
        Read,
        &[
            "select N'a ; delete from t -- '",
            "select \"a ; delete from t\"",
            "select \"a\"\"; delete from t\"",
            "select [a ; delete from t]",
            "select [delete], [exec], [into] from [update]",
            "select 1 as [go]",
        ],
    );
    // A backslash is no escape: this string ends at the second quote and `delete` is real.
    assert_eq!(classify(T, "select 'a\\' ; delete from t -- '"), Write);
    assert_eq!(classify(T, "select N'a\\'; delete from t; --'"), Write);
    // A doubled bracket is no end of the name; a single one is.
    assert_eq!(classify(T, "select [a]]; delete from t; --]"), Read);
    assert_eq!(classify(T, "select [a]; delete from t; --]"), Write);
    // Nested comments hide the `delete`; a non-nested reading would not.
    assert_eq!(classify(T, "select 1 /* /* */ delete from t */"), Read);
    assert_eq!(classify(T, "select 1 /* /* */ */ delete from t"), Write);
    assert_eq!(classify(T, "select 1 /* /* */ delete from t"), Unknown);
    // `#` is no comment.
    assert_eq!(classify(T, "select 1 # ; delete from t"), Unknown);
}

#[test]
fn only_known_functions_are_read() {
    check(
        Read,
        &[
            "select len(a), datalength(a), charindex('a', b), upper(a) from t",
            "select convert(nvarchar(10), a), try_cast(a as int), try_convert(int, a)",
            "select dateadd(day, 1, getutcdate()), datepart(year, a), eomonth(a)",
            "select cast(a as nvarchar(max)), cast(a as datetime2(3)), cast(a as datetimeoffset(7))",
            "select newid(), object_id('t'), schema_name(), db_name(), db_id()",
            "select count_big(*), checksum(a, b), binary_checksum(*) from t",
            "select value from string_split(a, ',')",
            "select a from t for xml path('r'), root('x')",
            "select distinct top (3) a from t",
            "select all top (3) a from t",
            "select a from t where a in (select top (1) a from u)",
            "select a from t order by a offset 5 rows fetch next 5 rows only",
        ],
    );
    check(
        Unknown,
        &[
            "select f(1)",
            "select dbo.f(1)",
            "select [f](1)",
            "select \"f\"(1)",
            "select sys.fn_my(1)",
            "select a, top (1)",
            "select 1 where option (x)",
            "select * from t (nolock)",
            "select openrowset(a)",
            "select * from opendatasource(a)",
            "select raiserror(1)",
            "select geography::STGeomFromText('x', 4326)",
            "select xp_cmdshell('x')",
        ],
    );
}

#[test]
fn isolation_and_lock_words_are_writes() {
    for w in [
        "updlock",
        "xlock",
        "holdlock",
        "tablock",
        "tablockx",
        "serializable",
        "repeatableread",
        "UPDLOCK",
        "TabLockX",
    ] {
        let sql = format!("select * from t with ({w})");
        assert_eq!(classify(T, &sql), Write, "{sql}");
    }
    // Harmless hints stay readable.
    assert_eq!(classify(T, "select * from t with (nolock, readpast)"), Read);
}

#[test]
fn every_other_dialect_is_unchanged() {
    // The T-SQL leads are not leads elsewhere.
    for d in [Dialect::Postgres, Dialect::MySql, Dialect::Sqlite] {
        for sql in [
            "dbcc checkdb",
            "waitfor delay '00:00:01'",
            "declare @x int",
            "print 'x'",
            "deny select on t to u",
            "bulk insert t from 'f'",
        ] {
            assert_eq!(classify(d, sql), Unknown, "{d:?}: {sql}");
        }
    }
}
