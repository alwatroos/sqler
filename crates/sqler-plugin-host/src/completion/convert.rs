// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Conversion between the plain completion types and the WIT ones. Text from a plugin is cut to one line and capped.

use sqler_plugin_api::{
    CompletionItem, CompletionKind, CompletionRequest, MAX_COMPLETION_DETAIL_BYTES,
    MAX_COMPLETION_INSERT_BYTES, MAX_COMPLETION_ITEMS, MAX_COMPLETION_LABEL_BYTES,
    MAX_COMPLETION_STATEMENT_BYTES,
};

use crate::error::{CallError, ErrorCode, GuestError};
use crate::runtime::wit_exports::completion as wit;
use crate::ui::limits::one_line;

fn invalid(message: &str) -> CallError {
    CallError::Guest(GuestError {
        code: ErrorCode::InvalidArgument,
        message: message.to_owned(),
    })
}

/// The WIT request. The connection is never sent. `Err` is the call to return, before any guest code runs.
pub(super) fn request_to_wit(
    request: CompletionRequest,
) -> Result<wit::CompletionRequest, CallError> {
    if request.statement.len() > MAX_COMPLETION_STATEMENT_BYTES {
        return Err(invalid("the statement is too long for completion"));
    }
    if !request.statement.is_char_boundary(request.cursor)
        || request.cursor > request.statement.len()
    {
        return Err(invalid("the cursor is not inside the statement"));
    }
    let cursor = u32::try_from(request.cursor)
        .map_err(|_| invalid("the cursor is not inside the statement"))?;
    Ok(wit::CompletionRequest {
        connection: None,
        driver: request.driver,
        text: request.statement,
        cursor,
    })
}

fn kind_from_wit(kind: wit::CompletionKind) -> CompletionKind {
    match kind {
        wit::CompletionKind::Keyword => CompletionKind::Keyword,
        wit::CompletionKind::Table => CompletionKind::Table,
        wit::CompletionKind::Column => CompletionKind::Column,
        wit::CompletionKind::Function => CompletionKind::Function,
        wit::CompletionKind::Snippet => CompletionKind::Snippet,
        wit::CompletionKind::Other => CompletionKind::Other,
    }
}

/// Cleans, filters and caps a guest answer.
pub(super) fn items_from_wit(items: Vec<wit::CompletionItem>) -> Vec<CompletionItem> {
    items
        .into_iter()
        .filter_map(|item| {
            let label = one_line(&item.label, MAX_COMPLETION_LABEL_BYTES);
            if label.is_empty() {
                return None;
            }
            let insert = one_line(&item.insert_text, MAX_COMPLETION_INSERT_BYTES);
            let insert_text = if insert.is_empty() {
                label.clone()
            } else {
                insert
            };
            let detail = item
                .detail
                .map(|d| one_line(&d, MAX_COMPLETION_DETAIL_BYTES))
                .filter(|d| !d.is_empty());
            Some(CompletionItem {
                label,
                kind: kind_from_wit(item.kind),
                insert_text,
                detail,
            })
        })
        .take(MAX_COMPLETION_ITEMS)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(label: &str, insert: &str, detail: Option<&str>) -> wit::CompletionItem {
        wit::CompletionItem {
            label: label.to_owned(),
            insert_text: insert.to_owned(),
            kind: wit::CompletionKind::Table,
            detail: detail.map(str::to_owned),
        }
    }

    fn req(statement: &str, cursor: usize) -> CompletionRequest {
        CompletionRequest {
            driver: Some("sqlite".to_owned()),
            statement: statement.to_owned(),
            cursor,
        }
    }

    #[test]
    fn the_request_carries_no_connection() {
        let r = request_to_wit(req("select 1", 3)).unwrap();
        assert_eq!(r.connection, None);
        assert_eq!(r.driver.as_deref(), Some("sqlite"));
        assert_eq!(r.text, "select 1");
        assert_eq!(r.cursor, 3);
    }

    #[test]
    fn a_bad_request_is_refused() {
        let long = "a".repeat(MAX_COMPLETION_STATEMENT_BYTES + 1);
        for r in [req(&long, 0), req("abc", 4), req("\u{e9}", 1)] {
            assert!(
                matches!(request_to_wit(r), Err(CallError::Guest(g)) if g.code == ErrorCode::InvalidArgument)
            );
        }
        let max = "a".repeat(MAX_COMPLETION_STATEMENT_BYTES);
        assert!(request_to_wit(req(&max, MAX_COMPLETION_STATEMENT_BYTES)).is_ok());
        assert!(request_to_wit(req("\u{e9}", 2)).is_ok());
    }

    #[test]
    fn text_is_one_clean_line_within_the_caps() {
        let long = "\u{e9}".repeat(MAX_COMPLETION_LABEL_BYTES);
        let out = items_from_wit(vec![raw(
            &format!("a\nb\u{1b}[0m\u{202e}c{long}"),
            &"x".repeat(MAX_COMPLETION_INSERT_BYTES + 10),
            Some(&"d".repeat(MAX_COMPLETION_DETAIL_BYTES + 10)),
        )]);
        assert_eq!(out.len(), 1);
        assert!(out[0].label.starts_with("a b[0mc"), "{:?}", out[0].label);
        assert!(!out[0].label.contains(['\n', '\u{1b}', '\u{202e}']));
        assert!(out[0].label.len() <= MAX_COMPLETION_LABEL_BYTES);
        assert_eq!(out[0].insert_text.len(), MAX_COMPLETION_INSERT_BYTES);
        assert_eq!(
            out[0].detail.as_ref().unwrap().len(),
            MAX_COMPLETION_DETAIL_BYTES
        );
    }

    #[test]
    fn empty_label_drops_empty_insert_falls_back_empty_detail_is_none() {
        let out = items_from_wit(vec![
            raw(" \n\u{1b}", "x", None),
            raw("name", "", Some(" ")),
        ]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].insert_text, "name");
        assert_eq!(out[0].detail, None);
    }

    #[test]
    fn the_answer_is_capped_after_filtering() {
        let mut items: Vec<_> = (0..10).map(|_| raw("", "", None)).collect();
        items.extend((0..MAX_COMPLETION_ITEMS + 20).map(|i| raw(&format!("t{i}"), "", None)));
        let out = items_from_wit(items);
        assert_eq!(out.len(), MAX_COMPLETION_ITEMS);
        assert_eq!(out[0].label, "t0");
    }

    #[test]
    fn kinds_map_one_to_one() {
        use wit::CompletionKind as W;
        let pairs = [
            (W::Keyword, CompletionKind::Keyword),
            (W::Table, CompletionKind::Table),
            (W::Column, CompletionKind::Column),
            (W::Function, CompletionKind::Function),
            (W::Snippet, CompletionKind::Snippet),
            (W::Other, CompletionKind::Other),
        ];
        for (w, k) in pairs {
            assert_eq!(kind_from_wit(w), k);
        }
    }
}
