// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Properties of statement classification.

use proptest::prelude::*;
use sqler_sql::{Dialect, StatementClass, classify, classify_each};

const FRAGMENTS: &[&str] = &[
    ";",
    "'",
    "''",
    "\"",
    "`",
    "$$",
    "$a$",
    "--",
    "#",
    "/*",
    "*/",
    "/*!",
    "(",
    ")",
    "[",
    "]",
    "select",
    "begin",
    "end",
    "case",
    "delimiter //\n",
    "create trigger t ",
    " ",
    "\n",
    ":=",
    "\\",
    "é",
    "名",
    "\0",
    "insert",
    "into",
    "pragma",
    "explain",
    "with",
    "\nGO\n",
    "N'",
    "[[",
    "]]",
    "@@",
    "exec ",
    "waitfor ",
];

fn mixed() -> impl Strategy<Value = String> {
    prop_oneof![
        any::<String>(),
        prop::collection::vec(prop::sample::select(FRAGMENTS), 0..40).prop_map(|v| v.concat()),
    ]
}

fn dialect() -> impl Strategy<Value = Dialect> {
    prop::sample::select(Dialect::ALL.to_vec())
}

/// Read statements written as space-separated bare tokens, so a word can be inserted between any two.
const READS: &[&str] = &[
    "select 1",
    "select a , b from t where a = 1 order by b",
    "select count ( * ) from t group by a having count ( * ) > 1",
    "with x as ( select 1 ) select * from x",
    "values ( 1 , 2 )",
    "table t",
    "show tables",
    "explain select a from t",
    "describe t",
    "select * from a join b on a.x = b.x limit 5",
];

const WRITES: &[&str] = &[
    "insert into t values (1)",
    "update t set a = 1",
    "delete from t",
    "merge into t using s on true when matched then delete",
    "replace into t values (1)",
    "copy t from stdin",
    "load data infile 'x' into table t",
    "create table x (a int)",
    "alter table t add b int",
    "drop table t",
    "truncate t",
    "grant select on t to u",
    "call p()",
    "do 1",
    "execute p",
    "select nextval('s')",
    "select pg_sleep(1)",
    "select 1 into t2",
    "select * from t for update",
    "frobnicate",
    "set x = 1",
    "begin",
    "vacuum",
    "select 'unterminated",
];

/// Words that make a read statement not read when they appear as a bare token.
const BARE_WORDS: &[&str] = &[
    "insert", "update", "delete", "merge", "upsert", "into", "copy", "load", "lock", "share",
    "create", "alter", "drop", "truncate", "rename", "comment", "grant", "revoke", "security",
    "import", "reindex", "call", "do", "exec", "execute", "INSERT", "Drop", "dElEtE", "CALL",
];

const WRITE_LEADS: &[&str] = &[
    "insert",
    "update",
    "delete",
    "merge",
    "replace",
    "upsert",
    "copy",
    "load",
    "refresh",
    "create",
    "alter",
    "drop",
    "truncate",
    "rename",
    "comment",
    "grant",
    "revoke",
    "security",
    "import",
    "reindex",
    "begin",
    "set",
    "vacuum",
    "lock",
    "attach",
    "call",
    "do",
    "exec",
    "execute",
    "pragma",
    "analyze",
    "cluster",
    "flush",
    "kill",
    "detach",
    "optimize",
    "repair",
    "checkpoint",
    "notify",
    "install",
    "purge",
];

proptest! {
    #[test]
    fn never_panics(d in dialect(), text in mixed()) {
        let _ = classify(d, &text);
        let _ = classify_each(d, &text);
    }

    #[test]
    fn classify_is_max_over_statements(d in dialect(), text in mixed()) {
        let each = classify_each(d, &text);
        let max = each.iter().map(|(_, c)| *c).max().unwrap_or(StatementClass::Unknown);
        prop_assert_eq!(classify(d, &text), max);
    }

    #[test]
    fn write_after_read_is_never_read(
        d in dialect(),
        r in prop::sample::select(READS),
        w in prop::sample::select(WRITES),
    ) {
        let text = format!("{r}; {w}");
        prop_assert!(!classify(d, &text).is_read_only(), "{}", text);
        let text = format!("{w}; {r}");
        prop_assert!(!classify(d, &text).is_read_only(), "{}", text);
    }

    #[test]
    fn write_word_inserted_anywhere_is_never_read(
        d in dialect(),
        r in prop::sample::select(READS),
        w in prop::sample::select(BARE_WORDS),
        at in 0usize..32,
    ) {
        let mut toks: Vec<&str> = r.split(' ').collect();
        let at = at % (toks.len() + 1);
        toks.insert(at, w);
        let text = toks.join(" ");
        prop_assert!(!classify(d, &text).is_read_only(), "{}", text);
    }

    #[test]
    fn write_keyword_prefix_is_never_read(
        d in dialect(),
        kw in prop::sample::select(WRITE_LEADS),
        rest in mixed(),
    ) {
        let text = format!("{kw} {rest}");
        prop_assert!(!classify(d, &text).is_read_only(), "{}", text);
        let text = format!("{}\n{rest}", kw.to_uppercase());
        prop_assert!(!classify(d, &text).is_read_only(), "{}", text);
    }

    #[test]
    fn unterminated_text_is_never_read(d in dialect(), q in prop::sample::select(vec!["'", "\"", "/*"]), rest in "[a-z ]{0,20}") {
        let text = format!("select 1 {q}{rest}");
        prop_assert!(!classify(d, &text).is_read_only(), "{}", text);
    }
}

/// T-SQL statements that are not read. Whitespace separates T-SQL statements as well as `;`, so
/// each one is also tried glued to a read with no separator.
const TSQL_WRITES: &[&str] = &[
    "dbcc checkdb",
    "backup database d to disk = 'x'",
    "restore database d from disk = 'x'",
    "bulk insert t from 'f'",
    "reconfigure",
    "shutdown",
    "kill 52",
    "checkpoint",
    "send on conversation @h ('x')",
    "end conversation @h",
    "move conversation @h to @g",
    "waitfor ( receive top ( 1 ) * from q )",
    "waitfor delay '00:00:01'",
    "updatetext t.c @p 0 0 'x'",
    "writetext t.c @p 'x'",
    "deny select on t to u",
    "enable trigger tr on t",
    "disable trigger tr on t",
    "add signature to p by certificate c",
    "exec p",
    "execute ( 'drop table t' )",
    "exec sp_executesql N'delete from t'",
    "declare @x int = 1",
    "print 'x'",
    "save tran s",
    "set nocount on",
    "set transaction isolation level serializable",
    "use master",
    "begin tran",
    "commit tran",
    "rollback tran",
    "setuser 'x'",
    "revert",
    "open symmetric key k decryption by certificate c",
    "select a into #t from b",
    "select * from t with ( updlock )",
    "select next value for s",
    "create procedure p as select 1",
    "truncate table t",
    "if 1 = 1 delete from t",
];

proptest! {
    #[test]
    fn go_line_between_read_and_write_is_never_read(
        r in prop::sample::select(READS),
        w in prop::sample::select(WRITES),
    ) {
        let text = format!("{r}\nGO\n{w}");
        prop_assert!(!classify(Dialect::TSql, &text).is_read_only(), "{}", text);
        let text = format!("{w}\nGO\n{r}");
        prop_assert!(!classify(Dialect::TSql, &text).is_read_only(), "{}", text);
    }

    #[test]
    fn tsql_statements_are_never_read_next_to_a_read(
        r in prop::sample::select(READS),
        w in prop::sample::select(TSQL_WRITES),
        glue in prop::sample::select(vec!["; ", " ", "\n", "\nGO\n", "\nGO 2\n", " /* x */ ", "\r\n"]),
    ) {
        let text = format!("{r}{glue}{w}");
        prop_assert!(!classify(Dialect::TSql, &text).is_read_only(), "{}", text);
        let text = format!("{w}{glue}{r}");
        prop_assert!(!classify(Dialect::TSql, &text).is_read_only(), "{}", text);
    }

    #[test]
    fn tsql_write_lead_is_never_read(w in prop::sample::select(TSQL_WRITES), rest in mixed()) {
        let text = format!("{w} {rest}");
        prop_assert!(!classify(Dialect::TSql, &text).is_read_only(), "{}", text);
        let text = format!("{}\n{rest}", w.to_uppercase());
        prop_assert!(!classify(Dialect::TSql, &text).is_read_only(), "{}", text);
    }
}

#[test]
fn read_templates_are_read() {
    for d in Dialect::ALL {
        for r in READS {
            assert!(classify(d, r).is_read_only(), "{d:?}: {r}");
        }
    }
}

proptest! {
    #[test]
    fn mysql_executable_comment_is_never_read(r in prop::sample::select(READS), w in "[a-z0-9 ]{0,12}") {
        let text = format!("{r} /*!50000 {w} */");
        prop_assert!(!classify(Dialect::MySql, &text).is_read_only(), "{}", text);
    }

    #[test]
    fn unterminated_dollar_quote_is_never_read(r in prop::sample::select(READS), w in "[a-z ]{0,12}") {
        let text = format!("{r} $$ {w}");
        prop_assert!(!classify(Dialect::Postgres, &text).is_read_only(), "{}", text);
    }
}

proptest! {
    #[test]
    fn unterminated_backtick_is_never_read(r in prop::sample::select(READS), w in "[a-z ]{0,12}") {
        for d in [Dialect::MySql, Dialect::Sqlite] {
            let text = format!("{r} `{w}");
            prop_assert!(!classify(d, &text).is_read_only(), "{}", text);
        }
    }
}
