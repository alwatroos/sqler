// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Properties of identifier quoting.

use proptest::prelude::*;
use sqler_core::{DbObject, ObjectKind, ObjectPath};
use sqler_sql::{DdlError, Dialect, qualified_name, quote_identifier};

fn quote_char(dialect: Dialect) -> char {
    match dialect {
        Dialect::MySql => '`',
        _ => '"',
    }
}

/// Decodes one quoted token from the start of `s`; returns the name and the rest.
/// `None` if the token is malformed or ends early.
fn decode_token(dialect: Dialect, s: &str) -> Option<(String, &str)> {
    let q = quote_char(dialect);
    let mut chars = s.char_indices();
    if chars.next()?.1 != q {
        return None;
    }
    let mut out = String::new();
    let mut iter = chars.peekable();
    while let Some((i, c)) = iter.next() {
        if c == q {
            if let Some(&(_, next)) = iter.peek()
                && next == q
            {
                out.push(q);
                iter.next();
                continue;
            }
            return Some((out, &s[i + c.len_utf8()..]));
        }
        out.push(c);
    }
    None
}

fn decode_whole(dialect: Dialect, s: &str) -> Option<String> {
    let (name, rest) = decode_token(dialect, s)?;
    rest.is_empty().then_some(name)
}

fn strategy() -> impl Strategy<Value = String> {
    prop_oneof![any::<String>(), "[\"`'; .\\-a-zA-Z0-9 ]{0,24}"]
}

proptest! {
    #[test]
    fn round_trip(name in strategy()) {
        prop_assume!(!name.is_empty() && !name.contains('\0'));
        for dialect in Dialect::ALL {
            let quoted = quote_identifier(dialect, &name).unwrap();
            prop_assert_eq!(decode_whole(dialect, &quoted), Some(name.clone()));
        }
    }

    #[test]
    fn invalid_names_are_rejected(prefix in strategy(), suffix in strategy()) {
        for dialect in Dialect::ALL {
            let with_nul = format!("{prefix}\0{suffix}");
            prop_assert_eq!(
                quote_identifier(dialect, &with_nul),
                Err(DdlError::InvalidIdentifier { name: with_nul.clone() })
            );
        }
        prop_assert!(quote_identifier(Dialect::Postgres, "").is_err());
    }

    #[test]
    fn qualified_names_have_two_tokens(schema in strategy(), name in strategy()) {
        prop_assume!(!schema.is_empty() && !schema.contains('\0'));
        prop_assume!(!name.is_empty() && !name.contains('\0'));
        for dialect in Dialect::ALL {
            let obj = DbObject::new(
                ObjectKind::Table,
                ObjectPath::root().in_schema(schema.clone()),
                name.clone(),
            );
            let text = qualified_name(dialect, &obj).unwrap();
            let (first, rest) = decode_token(dialect, &text).unwrap();
            let rest = rest.strip_prefix('.').unwrap();
            let second = decode_whole(dialect, rest).unwrap();
            prop_assert_eq!((first, second), (schema.clone(), name.clone()));
        }
    }
}
