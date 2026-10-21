// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The exported tool definitions.
#![allow(clippy::unwrap_used)]

mod common;

use std::collections::HashSet;
use std::sync::Arc;

use common::{FunctionSource, limits};
use sqler_ai_tools::{ToolSet, UiHost, names};

struct NoUi;

impl UiHost for NoUi {
    fn open_in_editor<'a>(
        &'a self,
        _: Option<&'a str>,
        _: &'a str,
    ) -> sqler_ai_api::BoxFuture<'a, Result<(), sqler_ai_tools::ToolError>> {
        Box::pin(std::future::ready(Ok(())))
    }

    fn open_results_tab<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: Option<&'a str>,
    ) -> sqler_ai_api::BoxFuture<'a, Result<(), sqler_ai_tools::ToolError>> {
        Box::pin(std::future::ready(Ok(())))
    }
}

#[tokio::test(flavor = "current_thread")]
async fn definitions_are_well_formed() {
    let ts = ToolSet::new(FunctionSource::new(), limits());
    let plain = ts.definitions();
    let with_ui = ts.with_ui_host(Arc::new(NoUi)).definitions();
    for set in [&plain, &with_ui] {
        let unique: HashSet<_> = set.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(unique.len(), set.len(), "names are unique");
        for d in set {
            assert!(!d.description.is_empty(), "{}", d.name);
            let schema = &d.parameters;
            assert_eq!(schema["type"], "object", "{}", d.name);
            assert_eq!(schema["additionalProperties"], false, "{}", d.name);
            assert!(schema["properties"].is_object(), "{}", d.name);
            let required = schema["required"].as_array().unwrap();
            for r in required {
                assert!(schema["properties"][r.as_str().unwrap()].is_object());
            }
        }
    }
    let plain_names: HashSet<_> = plain.iter().map(|d| d.name.as_str()).collect();
    for name in [
        names::LIST_CONNECTIONS,
        names::LIST_SCHEMAS,
        names::LIST_TABLES,
        names::DESCRIBE_TABLE,
        names::DESCRIBE_FUNCTION,
        names::SEARCH_SCHEMA,
        names::SAMPLE_ROWS,
        names::RUN_QUERY,
        names::EXPLAIN_QUERY,
    ] {
        assert!(plain_names.contains(name), "{name}");
    }
    assert!(!plain_names.contains(names::OPEN_IN_EDITOR));
    assert!(!plain_names.contains(names::OPEN_RESULTS_TAB));
    assert!(!plain_names.contains(names::SEARCH_DATA));
    let ui_names: HashSet<_> = with_ui.iter().map(|d| d.name.as_str()).collect();
    assert!(ui_names.contains(names::OPEN_IN_EDITOR));
    assert!(ui_names.contains(names::OPEN_RESULTS_TAB));
    assert!(!ui_names.contains(names::SEARCH_DATA));
    assert_eq!(with_ui.len(), plain.len() + 2);
    assert_eq!(sqler_ai_tools::definitions(true), with_ui);
}

#[test]
fn futures_are_send() {
    fn assert_send<T: Send>(_: &T) {}
    let ts = ToolSet::new(
        // never polled: only the type matters
        Arc::new(NullSource),
        limits(),
    );
    let call = sqler_ai_api::ToolCall::new("1", "x", "{}");
    let fut = ts.call_detailed(&call);
    assert_send(&fut);
    let fut = ts.call(&call);
    assert_send(&fut);
}

struct NullSource;

impl sqler_ai_tools::SessionSource for NullSource {
    fn connections(&self) -> Vec<sqler_ai_tools::ConnectionInfo> {
        Vec::new()
    }

    fn catalog(&self, _: &str) -> Option<sqler_catalog::Catalog> {
        None
    }

    fn acquire<'a>(
        &'a self,
        c: &'a str,
    ) -> sqler_ai_api::BoxFuture<
        'a,
        Result<Box<dyn sqler_driver_api::Session>, sqler_ai_tools::ToolError>,
    > {
        Box::pin(std::future::ready(Err(
            sqler_ai_tools::ToolError::UnknownConnection(c.to_owned()),
        )))
    }
}
