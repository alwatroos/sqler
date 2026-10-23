// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Completer fixture plugin (`completion-plugin` world). The statement text picks the answer, so the host tests
//! (`crates/sqler-plugin-host/tests/guests/completion.rs`) can drive the call: `spin` never returns, `trap` traps,
//! `flood` returns 500 unclean items, `echo` returns what the request carried, anything else returns two items.

// The wit-bindgen macro expands to `unsafe` FFI glue and undocumented items (AGENTS.md section 4 exception),
// so the generated code and the exports live in one module with the lints relaxed.
#[allow(unsafe_code, missing_docs, clippy::all, clippy::pedantic)]
mod bindings {
    wit_bindgen::generate!({
        path: "../../../crates/sqler-plugin-api/wit",
        world: "completion-plugin",
    });

    use self::exports::sqler::plugin::completion::{
        CompletionItem, CompletionKind, CompletionRequest, Guest,
    };
    use self::exports::sqler::plugin::lifecycle;

    struct Completer;

    impl lifecycle::Guest for Completer {
        fn init() -> Result<(), lifecycle::PluginError> {
            Ok(())
        }
    }

    fn item(
        label: &str,
        kind: CompletionKind,
        insert: &str,
        detail: Option<&str>,
    ) -> CompletionItem {
        CompletionItem {
            label: label.to_owned(),
            insert_text: insert.to_owned(),
            kind,
            detail: detail.map(str::to_owned),
        }
    }

    impl Guest for Completer {
        fn complete(request: CompletionRequest) -> Vec<CompletionItem> {
            match request.text.as_str() {
                "spin" => loop {
                    std::hint::black_box(());
                },
                "trap" => {
                    core::arch::wasm32::unreachable();
                }
                "flood" => (0..500)
                    .map(|i| {
                        let label = format!("f{i}\n\u{1b}[31m{}", "x".repeat(300));
                        item(&label, CompletionKind::Other, "", Some("a\tb\r\nc"))
                    })
                    .collect(),
                "echo" => {
                    let detail = format!(
                        "driver={:?};connection={:?};cursor={};text={}",
                        request.driver, request.connection, request.cursor, request.text
                    );
                    vec![item("echo", CompletionKind::Other, "echo", Some(&detail))]
                }
                _ => vec![
                    item("fixture_one", CompletionKind::Keyword, "fixture_one", None),
                    item(
                        "fixture_two",
                        CompletionKind::Table,
                        "fixture_two",
                        Some("from fixture"),
                    ),
                ],
            }
        }
    }

    export!(Completer);
}
