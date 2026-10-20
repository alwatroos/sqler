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
    "insert", "update", "delete", "merge", "replace", "upsert", "copy", "load", "refresh",
    "create", "alter", "drop", "truncate", "rename", "comment", "grant", "revoke", "security",
    "import", "reindex", "begin", "set", "vacuum", "lock", "attach", "call", "do", "exec",
    "execute", "pragma",
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
