// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! What a statement does to the user's transaction, as far as the text shows (ADR-0033).
//!
//! `apply` opens its own `BEGIN`, so it must not run on a session the user left inside a
//! transaction (ADR-0030 makes that guard the caller's duty). The app cannot ask the server, so
//! the workspace follows the statements a run executes: [`tx_effect`] reads one statement text
//! and says whether it begins a transaction, ends one, or does neither.
//!
//! The reading errs toward "in a transaction", the direction in which a mistake only costs the
//! user a `COMMIT`:
//!
//! * a word it does not know leaves the flag as it was (`RELEASE`, `ROLLBACK TO`, `XA END`);
//! * a statement that could end a transaction only counts as an end when its dialect has it
//!   (`END` is a `PostgreSQL` and `SQLite` statement; `ABORT` is `PostgreSQL`'s);
//! * `PREPARE TRANSACTION` ends the session's transaction, but `COMMIT PREPARED` and
//!   `ROLLBACK PREPARED` act on another one and are neither;
//! * a value of `autocommit` it cannot read counts as a begin.
//!
//! Executable comments (`/*! ... */`, and `/*M! ... */` on `MySQL`) are part of the statement.

use sqler_sql::{Dialect, SegmentKind, Token, TokenKind, split, tokenize};

/// What a statement text does to the transaction state of its session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TxEffect {
    /// Nothing, or nothing the text shows.
    None,
    /// The session is (now) inside a transaction.
    Begin,
    /// The session is back in autocommit mode.
    End,
    /// `SET autocommit = 0` (or a value that cannot be read) on `MySQL`: the session is inside a
    /// transaction now, and the next one starts as soon as this one ends.
    AutocommitOff,
    /// `SET autocommit = 1` on `MySQL`: no transaction is open and none starts by itself.
    AutocommitOn,
}

/// One significant token, reduced to what the rules look at.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    /// A keyword or bare identifier, upper-cased.
    Word(String),
    /// A quoted identifier (`` `autocommit` ``, `"autocommit"`), unquoted and upper-cased.
    Quoted(String),
    /// A number, an operator, punctuation or a quoted string (its inner text).
    Other(String),
}

/// The effect of `sql` (one statement or a script) on the transaction state of its session.
///
/// For a script the last statement that has an effect decides, as it would when all of them run.
#[must_use]
pub(crate) fn tx_effect(dialect: Dialect, sql: &str) -> TxEffect {
    let mut result = TxEffect::None;
    // `SET autocommit = 0` is sticky: only `autocommit = 1` clears it, not `COMMIT` or `BEGIN`.
    let mut off = false;
    let mut any = false;
    for segment in split(dialect, sql) {
        if segment.kind != SegmentKind::Statement {
            continue;
        }
        any = true;
        let body = sql.get(segment.body.clone()).unwrap_or("");
        match statement_effect(dialect, body) {
            TxEffect::None => {}
            effect => {
                match effect {
                    TxEffect::AutocommitOff => off = true,
                    TxEffect::AutocommitOn => off = false,
                    _ => {}
                }
                result = effect;
            }
        }
    }
    if off {
        // `COMMIT` or `BEGIN` after `SET autocommit = 0`: the next transaction is open.
        return TxEffect::AutocommitOff;
    }
    if !any {
        // No statement found: read the whole text, so no keyword is lost to the splitter.
        return statement_effect(dialect, sql);
    }
    result
}

fn push_tokens(dialect: Dialect, text: &str, out: &mut Vec<Tok>, depth: u8) {
    for token in tokenize(dialect, text) {
        push_token(dialect, text, token, out, depth);
    }
}

fn push_token(dialect: Dialect, text: &str, token: Token, out: &mut Vec<Tok>, depth: u8) {
    let t = token.text(text);
    match token.kind {
        TokenKind::Whitespace | TokenKind::LineComment => {}
        TokenKind::BlockComment { .. } | TokenKind::ExecComment { .. } => {
            // `/*! ... */` and `/*M! ... */` run their content (`MariaDB` writes `M!`).
            let executable = matches!(token.kind, TokenKind::ExecComment { .. })
                || (dialect == Dialect::MySql && t.starts_with("/*M!"));
            if !executable || depth > 0 {
                return;
            }
            let inner = t
                .trim_start_matches("/*")
                .trim_start_matches('M')
                .trim_start_matches('!')
                .trim_start_matches(|c: char| c.is_ascii_digit());
            let inner = inner.strip_suffix("*/").unwrap_or(inner);
            push_tokens(dialect, inner, out, depth + 1);
        }
        TokenKind::QuotedIdent { .. } => out.push(Tok::Quoted(
            t.trim_matches(|c: char| c == '`' || c == '"' || c == '[' || c == ']')
                .to_ascii_uppercase(),
        )),
        TokenKind::Word => out.push(Tok::Word(t.to_ascii_uppercase())),
        TokenKind::Str { .. } => out.push(Tok::Other(
            t.trim_start_matches(|c: char| c.is_ascii_alphabetic() || c == '_')
                .trim_matches(|c: char| c == '\'' || c == '"')
                .to_ascii_uppercase(),
        )),
        _ => out.push(Tok::Other(t.to_ascii_uppercase())),
    }
}

fn is(t: &[Tok], i: usize, word: &str) -> bool {
    matches!(t.get(i), Some(Tok::Word(w)) if w == word)
}

fn statement_effect(dialect: Dialect, body: &str) -> TxEffect {
    let mut t = Vec::new();
    push_tokens(dialect, body, &mut t, 0);
    let Some(Tok::Word(first)) = t.first() else {
        return TxEffect::None;
    };
    match first.as_str() {
        "BEGIN" | "SAVEPOINT" => TxEffect::Begin,
        "START" if is(&t, 1, "TRANSACTION") => TxEffect::Begin,
        "COMMIT" | "ROLLBACK" => finish(&t, TxEffect::End),
        "END" if matches!(dialect, Dialect::Postgres | Dialect::Sqlite) => {
            finish(&t, TxEffect::End)
        }
        "ABORT" if dialect == Dialect::Postgres => finish(&t, TxEffect::End),
        "PREPARE" if dialect == Dialect::Postgres && is(&t, 1, "TRANSACTION") => TxEffect::End,
        "XA" if is(&t, 1, "START") || is(&t, 1, "BEGIN") => TxEffect::Begin,
        "XA" if is(&t, 1, "COMMIT") || is(&t, 1, "ROLLBACK") => TxEffect::End,
        "SET" => set_effect(dialect, &t),
        _ => TxEffect::None,
    }
}

/// `COMMIT`, `ROLLBACK`, `END` and `ABORT`: `AND CHAIN` starts the next transaction at once,
/// `ROLLBACK TO` and the `PREPARED` forms leave the current one alone.
fn finish(t: &[Tok], plain: TxEffect) -> TxEffect {
    let mut i = 1;
    if is(t, i, "WORK") || is(t, i, "TRANSACTION") {
        i += 1;
    }
    if is(t, i, "TO") || is(t, i, "PREPARED") {
        return TxEffect::None;
    }
    let mut chain = false;
    for (k, tok) in t.iter().enumerate().skip(i) {
        if matches!(tok, Tok::Word(w) if w == "CHAIN") {
            chain = !is(t, k - 1, "NO");
        }
    }
    if chain { TxEffect::Begin } else { plain }
}

/// `SET [SESSION] autocommit = value`, also `SET @@autocommit` and `@@session.autocommit`, among
/// other assignments. The last one decides. `GLOBAL` and `PERSIST` do not touch the session.
fn set_effect(dialect: Dialect, t: &[Tok]) -> TxEffect {
    if t.iter().any(|tok| {
        matches!(tok, Tok::Word(w)
            if matches!(w.as_str(), "GLOBAL" | "PERSIST" | "PERSIST_ONLY")
                || w.starts_with("@@GLOBAL")
                || w.starts_with("@@PERSIST"))
    }) {
        return TxEffect::None;
    }
    let mut result = TxEffect::None;
    for i in 1..t.len() {
        let named = matches!(&t[i], Tok::Word(w) | Tok::Quoted(w)
            if w == "AUTOCOMMIT" || w.ends_with("@@AUTOCOMMIT") || w.ends_with(".AUTOCOMMIT"));
        if !named {
            continue;
        }
        let assigns = is(t, i + 1, "TO")
            || matches!(t.get(i + 1), Some(Tok::Other(o)) if o == "=" || o == ":=");
        if !assigns {
            continue;
        }
        let value = match t.get(i + 2) {
            Some(Tok::Word(v) | Tok::Other(v) | Tok::Quoted(v)) => v.as_str(),
            None => "",
        };
        result = match value {
            "1" | "ON" | "TRUE" if dialect == Dialect::MySql => TxEffect::AutocommitOn,
            // "0", "OFF", "FALSE", and a value that cannot be read (err toward a transaction).
            _ if dialect == Dialect::MySql => TxEffect::AutocommitOff,
            _ => TxEffect::Begin,
        };
    }
    result
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    const PG: Dialect = Dialect::Postgres;
    const MY: Dialect = Dialect::MySql;
    const LITE: Dialect = Dialect::Sqlite;

    use TxEffect::{AutocommitOff, AutocommitOn, Begin, End, None as Nothing};

    #[test]
    fn table_of_statements() {
        let table: &[(Dialect, &str, TxEffect)] = &[
            (PG, "BEGIN", Begin),
            (PG, "begin work", Begin),
            (PG, "BEGIN ISOLATION LEVEL SERIALIZABLE", Begin),
            (PG, "START TRANSACTION READ ONLY", Begin),
            (PG, "SAVEPOINT a", Begin),
            (PG, "COMMIT", End),
            (PG, "commit work", End),
            (PG, "COMMIT AND CHAIN", Begin),
            (PG, "ROLLBACK AND CHAIN", Begin),
            (PG, "COMMIT AND NO CHAIN", End),
            (PG, "ROLLBACK", End),
            (PG, "ROLLBACK TRANSACTION", End),
            (PG, "ROLLBACK TO SAVEPOINT a", Nothing),
            (PG, "rollback to a", Nothing),
            (PG, "ROLLBACK WORK TO SAVEPOINT a", Nothing),
            (PG, "RELEASE SAVEPOINT a", Nothing),
            (PG, "END", End),
            (PG, "END TRANSACTION", End),
            (PG, "ABORT", End),
            (PG, "PREPARE TRANSACTION 'x'", End),
            (PG, "COMMIT PREPARED 'x'", Nothing),
            (PG, "ROLLBACK PREPARED 'x'", Nothing),
            (PG, "SELECT 1", Nothing),
            (PG, "SELECT 'BEGIN'", Nothing),
            (PG, "select \"begin\" from t", Nothing),
            (PG, "SET search_path = public", Nothing),
            (PG, "SET TRANSACTION READ ONLY", Nothing),
            (LITE, "BEGIN", Begin),
            (LITE, "BEGIN IMMEDIATE TRANSACTION", Begin),
            (LITE, "SAVEPOINT s", Begin),
            (LITE, "COMMIT", End),
            (LITE, "END", End),
            (LITE, "END TRANSACTION", End),
            (LITE, "ROLLBACK", End),
            (LITE, "ROLLBACK TO s", Nothing),
            (LITE, "ROLLBACK TRANSACTION TO SAVEPOINT s", Nothing),
            (LITE, "RELEASE s", Nothing),
            (LITE, "ABORT", Nothing),
            (MY, "BEGIN", Begin),
            (MY, "BEGIN WORK", Begin),
            (MY, "START TRANSACTION WITH CONSISTENT SNAPSHOT", Begin),
            (MY, "SAVEPOINT s", Begin),
            (MY, "COMMIT", End),
            (MY, "COMMIT AND NO CHAIN RELEASE", End),
            (MY, "COMMIT AND CHAIN", Begin),
            (MY, "ROLLBACK", End),
            (MY, "ROLLBACK TO SAVEPOINT s", Nothing),
            (MY, "RELEASE SAVEPOINT s", Nothing),
            (MY, "END", Nothing),
            (MY, "ABORT", Nothing),
            (MY, "SET autocommit = 0", AutocommitOff),
            (MY, "SET autocommit=0", AutocommitOff),
            (MY, "SET SESSION autocommit = OFF", AutocommitOff),
            (MY, "SET @@autocommit = false", AutocommitOff),
            (MY, "SET @@session.autocommit := 0", AutocommitOff),
            (MY, "SET autocommit = 1", AutocommitOn),
            (MY, "SET autocommit = ON", AutocommitOn),
            (MY, "set @@session.autocommit = TRUE", AutocommitOn),
            (MY, "SET autocommit = @x", AutocommitOff),
            (MY, "SET sql_mode = '', autocommit = 0", AutocommitOff),
            (MY, "SET autocommit = 0, sql_mode = ''", AutocommitOff),
            (MY, "SET GLOBAL autocommit = 1", Nothing),
            (MY, "SET @@global.autocommit = 0", Nothing),
            (MY, "SET NAMES utf8mb4", Nothing),
            (MY, "XA START 'a'", Begin),
            (MY, "XA END 'a'", Nothing),
            (MY, "XA COMMIT 'a'", End),
            (MY, "/*!50000 BEGIN */", Begin),
            (MY, "/*M!100000 START TRANSACTION */", Begin),
            (PG, "SET autocommit = 0", Begin),
            // Only MySQL has autocommit to switch back on.
            (PG, "SET autocommit = 1", Begin),
            (MY, "SET `autocommit` = 0", AutocommitOff),
            (MY, "SET SESSION `autocommit`=1", AutocommitOn),
            (MY, "SET autocommit = 0; COMMIT", AutocommitOff),
            (MY, "SET autocommit = 0; BEGIN; COMMIT", AutocommitOff),
            (MY, "SET autocommit = 0; BEGIN", AutocommitOff),
            (
                MY,
                "SET autocommit = 0; COMMIT; SET autocommit = 1",
                AutocommitOn,
            ),
            (
                MY,
                "SET autocommit = 0; COMMIT; SET autocommit = 1",
                AutocommitOn,
            ),
        ];
        for (dialect, sql, want) in table {
            assert_eq!(tx_effect(*dialect, sql), *want, "{dialect:?}: {sql}");
        }
    }

    #[test]
    fn comments_case_and_terminators_do_not_hide_a_keyword() {
        for sql in [
            "/* x */ BEGIN",
            "-- note\nBEGIN;",
            "  bEgIn /* y */ ;  ",
            "/**/BEGIN/**/WORK",
        ] {
            for d in [PG, MY, LITE] {
                assert_eq!(tx_effect(d, sql), Begin, "{d:?}: {sql}");
            }
        }
        assert_eq!(tx_effect(PG, "COMMIT /* AND CHAIN */"), End);
        assert_eq!(tx_effect(PG, "COMMIT -- AND CHAIN\n"), End);
    }

    #[test]
    fn scripts_end_with_the_last_effect() {
        assert_eq!(
            tx_effect(PG, "BEGIN; INSERT INTO t VALUES (1); COMMIT"),
            End
        );
        assert_eq!(tx_effect(PG, "COMMIT; BEGIN; SELECT 1"), Begin);
        assert_eq!(tx_effect(LITE, "SELECT 1; SELECT 2"), Nothing);
        assert_eq!(
            tx_effect(MY, "SET autocommit=0; INSERT INTO t VALUES (1)"),
            AutocommitOff
        );
        assert_eq!(tx_effect(PG, ""), Nothing);
        assert_eq!(tx_effect(PG, "   -- only a comment"), Nothing);
    }

    #[test]
    fn strings_that_mention_keywords_are_not_statements() {
        assert_eq!(tx_effect(PG, "SELECT 'x'; SELECT 'COMMIT; BEGIN'"), Nothing);
        assert_eq!(tx_effect(PG, "SELECT $$BEGIN; COMMIT$$"), Nothing);
        assert_eq!(tx_effect(MY, "SELECT 'a\\'; COMMIT'"), Nothing);
    }

    /// One statement with its effect per dialect: `(sql with spaces between the tokens, effect)`.
    fn corpus(dialect: Dialect) -> Vec<(&'static str, TxEffect)> {
        let mut out = vec![
            ("BEGIN", Begin),
            ("BEGIN WORK", Begin),
            ("SAVEPOINT sp1", Begin),
            ("COMMIT", End),
            ("COMMIT WORK", End),
            ("ROLLBACK", End),
            ("ROLLBACK WORK", End),
            ("ROLLBACK TO sp1", Nothing),
            ("ROLLBACK TO SAVEPOINT sp1", Nothing),
            ("RELEASE SAVEPOINT sp1", Nothing),
            ("RELEASE sp1", Nothing),
            ("COMMIT AND CHAIN", Begin),
            ("ROLLBACK AND CHAIN", Begin),
            ("COMMIT AND NO CHAIN", End),
            ("SELECT 1", Nothing),
            ("INSERT INTO t VALUES ( 1 )", Nothing),
            ("SELECT 'BEGIN'", Nothing),
            ("SELECT 'COMMIT ; ROLLBACK'", Nothing),
        ];
        match dialect {
            Dialect::Postgres => out.extend([
                ("START TRANSACTION", Begin),
                ("START TRANSACTION ISOLATION LEVEL SERIALIZABLE", Begin),
                ("END", End),
                ("END TRANSACTION", End),
                ("ABORT", End),
                ("PREPARE TRANSACTION 'g'", End),
                ("COMMIT PREPARED 'g'", Nothing),
                ("ROLLBACK PREPARED 'g'", Nothing),
            ]),
            Dialect::MySql => out.extend([
                ("START TRANSACTION", Begin),
                ("SET autocommit = 0", AutocommitOff),
                ("SET SESSION autocommit = OFF", AutocommitOff),
                ("SET @@autocommit = 0", AutocommitOff),
                ("SET autocommit = 1", AutocommitOn),
                ("SET @@session.autocommit = ON", AutocommitOn),
                ("END", Nothing),
            ]),
            _ => out.extend([
                ("END", End),
                ("END TRANSACTION", End),
                ("BEGIN IMMEDIATE", Begin),
            ]),
        }
        out
    }

    fn dialects() -> impl Strategy<Value = Dialect> {
        prop_oneof![Just(PG), Just(MY), Just(LITE)]
    }

    /// Randomly changes the case of letters.
    fn recase(text: &str, bits: u64) -> String {
        text.chars()
            .enumerate()
            .map(|(i, c)| {
                if (bits >> (i % 64)) & 1 == 1 {
                    c.to_ascii_uppercase()
                } else {
                    c.to_ascii_lowercase()
                }
            })
            .collect()
    }

    const SEPARATORS: [&str; 6] = [" ", "  ", "\n", " /* c */ ", "\t", " -- c\n"];

    /// Joins the words of `sql` (not the ones inside a string) with random separators.
    fn spread(sql: &str, picks: &[usize]) -> String {
        if sql.contains('\'') {
            return sql.to_owned();
        }
        let mut out = String::new();
        for (i, word) in sql.split(' ').enumerate() {
            if i > 0 {
                out.push_str(SEPARATORS[picks[i % picks.len()] % SEPARATORS.len()]);
            }
            out.push_str(word);
        }
        out
    }

    proptest! {

        /// A script of statements from the corpus, decorated with case, comments and
        /// separators, has the effect of its last statement with an effect.
        #[test]
        fn scripts_follow_their_last_effect(
            dialect in dialects(),
            picks in proptest::collection::vec((0usize..64, any::<u64>()), 1..5),
            seps in proptest::collection::vec(0usize..6, 1..6),
            lead in 0usize..3,
            tail in any::<bool>(),
        ) {
            let corpus = corpus(dialect);
            let mut script = String::new();
            let mut want = Nothing;
            let mut off = false;
            for (n, (which, bits)) in picks.iter().enumerate() {
                let (sql, effect) = corpus[which % corpus.len()];
                let mut statement = recase(&spread(sql, &seps), *bits);
                if sql.contains('\'') {
                    // Keep the string literals as they are: their case is data.
                    statement = sql.to_owned();
                }
                if n > 0 {
                    script.push_str(" ; ");
                }
                script.push_str(match lead { 0 => "", 1 => "/* lead */ ", _ => "-- lead\n" });
                script.push_str(&statement);
                if effect != Nothing {
                    want = effect;
                    off = effect == AutocommitOff || (off && effect != AutocommitOn);
                }
                if off {
                    // `COMMIT` or `BEGIN` after `SET autocommit = 0`: the next transaction is open.
                    want = AutocommitOff;
                }
            }
            if tail {
                script.push(';');
            }
            prop_assert_eq!(tx_effect(dialect, &script), want, "{}", script);
        }

        /// Whatever the text, the reading does not panic.
        #[test]
        fn any_text_is_read(dialect in dialects(), text in "\\PC{0,80}") {
            let _ = tx_effect(dialect, &text);
        }

        /// A begin keyword at the start of a statement is never lost to what follows it.
        #[test]
        fn a_begin_is_always_seen(
            dialect in dialects(),
            tail in "[ a-zA-Z0-9_,=()]{0,30}",
            bits in any::<u64>(),
        ) {
            let sql = format!("{} {tail}", recase("begin", bits));
            prop_assert_eq!(tx_effect(dialect, &sql), Begin, "{}", sql);
        }
    }
}
