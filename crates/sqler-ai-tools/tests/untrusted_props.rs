// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Property: the untrusted-data envelope cannot be closed from inside, and loses nothing.
#![allow(clippy::unwrap_used)]

use proptest::prelude::*;
use serde_json::{Map, Value};
use sqler_ai_tools::{
    UNTRUSTED_CLOSE, UNTRUSTED_NOTICE, UNTRUSTED_OPEN, unwrap_untrusted, wrap_untrusted,
};

fn text() -> impl Strategy<Value = String> {
    prop_oneof![
        any::<String>(),
        Just(UNTRUSTED_CLOSE.to_owned()),
        Just(UNTRUSTED_OPEN.to_owned()),
        Just("<>\"'\\\n\r\t\u{0}".to_owned()),
        proptest::collection::vec(
            prop_oneof![
                Just("<".to_owned()),
                Just(">".to_owned()),
                Just("</untrusted-data>".to_owned()),
                Just("\"".to_owned()),
                Just("\n".to_owned()),
                Just("\u{0}".to_owned()),
                any::<String>(),
            ],
            0..6
        )
        .prop_map(|parts| parts.concat()),
    ]
}

fn json() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(Value::from),
        text().prop_map(Value::String),
    ];
    leaf.prop_recursive(4, 48, 6, |inner| {
        prop_oneof![
            proptest::collection::vec(inner.clone(), 0..6).prop_map(Value::Array),
            proptest::collection::vec((text(), inner), 0..6)
                .prop_map(|kv| Value::Object(kv.into_iter().collect::<Map<_, _>>())),
        ]
    })
}

proptest! {
    #[test]
    fn the_envelope_holds_for_any_value(value in json()) {
        let wrapped = wrap_untrusted(&value);
        let head = format!("{UNTRUSTED_NOTICE}\n{UNTRUSTED_OPEN}\n");
        let tail = format!("\n{UNTRUSTED_CLOSE}");
        prop_assert!(wrapped.starts_with(&head));
        prop_assert!(wrapped.ends_with(&tail));
        prop_assert_eq!(wrapped.matches(UNTRUSTED_OPEN).count(), 1);
        prop_assert_eq!(wrapped.matches(UNTRUSTED_CLOSE).count(), 1);
        let inner = unwrap_untrusted(&wrapped).unwrap();
        prop_assert!(!inner.contains('<') && !inner.contains('>'));
        let back: Value = serde_json::from_str(inner).unwrap();
        prop_assert_eq!(back, value);
    }
}
