// SPDX-License-Identifier: Apache-2.0
//! gpui-component code editor in SQL mode, plus the highlight-kind counter.

use std::collections::BTreeSet;
use std::ops::Range;
use std::sync::Mutex;

use gpui::{AppContext as _, Context, Entity, HighlightStyle, Window};
use gpui_component::highlighter::SyntaxHighlighter;
use gpui_component::input::{EditorState, HighlightStyleResolver, Rope};

/// Sample query: keywords, string and numeric literals, a comment, a CTE and a join.
pub const SAMPLE_SQL: &str = "\
-- Monthly revenue per region for shipped orders
WITH shipped AS (
    SELECT o.id,
           o.region,
           o.qty * o.price AS revenue,
           o.order_date
    FROM orders AS o
    WHERE o.shipped = TRUE
      AND o.order_date >= DATE '2020-01-01'
      AND o.note <> 'return pending'
),
monthly AS (
    SELECT region,
           date_trunc('month', order_date) AS month,
           SUM(revenue) AS total,
           COUNT(*) AS orders
    FROM shipped
    GROUP BY region, date_trunc('month', order_date)
)
SELECT m.region,
       m.month,
       m.total,
       m.orders,
       c.name AS top_customer,
       ROUND(m.total / NULLIF(m.orders, 0), 2) AS avg_order
FROM monthly AS m
INNER JOIN customers AS c
        ON c.region = m.region
       AND c.rank = 1
WHERE m.total > 1000.50
  AND m.orders >= 10
ORDER BY m.month DESC, m.total DESC
LIMIT 100;

-- Orders that never shipped, oldest first
SELECT id, region, order_date
FROM orders
WHERE shipped = FALSE
  AND order_date < DATE '2021-06-30'
ORDER BY order_date ASC
LIMIT 25;
";

/// Language name the editor is switched to.
pub const LANGUAGE: &str = "sql";

/// Creates the editor state in SQL mode with the sample query loaded.
pub fn new_editor(window: &mut Window, cx: &mut gpui::App) -> Entity<EditorState> {
    cx.new(|cx: &mut Context<EditorState>| {
        let mut state = EditorState::new(window, cx).language(LANGUAGE);
        state.set_value(SAMPLE_SQL, window, cx);
        state
    })
}

/// Records every highlight name the highlighter asks the theme about.
#[derive(Default)]
struct Recorder(Mutex<BTreeSet<String>>);

impl HighlightStyleResolver for Recorder {
    fn style(&self, name: &str) -> Option<HighlightStyle> {
        if let Ok(mut names) = self.0.lock() {
            names.insert(name.to_owned());
        }
        Some(HighlightStyle::default())
    }
}

/// Distinct highlight names on `text`. The editor wraps the same
/// `SyntaxHighlighter` type (see `highlighter/input_adapter.rs` in gpui-component),
/// but this runs a separate instance over `text`; it does not query the live
/// editor state.
#[must_use]
pub fn highlight_kinds(text: &str) -> BTreeSet<String> {
    let rope = Rope::from_str(text);
    let mut highlighter = SyntaxHighlighter::new(LANGUAGE);
    highlighter.update(None, &rope, None);
    let recorder = Recorder::default();
    let range: Range<usize> = 0..text.len();
    let _ = highlighter.styles(&range, &recorder);
    recorder.0.into_inner().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_sql_is_about_forty_lines() {
        let lines = SAMPLE_SQL.lines().count();
        assert!((35..=45).contains(&lines), "{lines} lines");
    }

    #[test]
    fn sample_sql_has_several_highlight_kinds() {
        let kinds = highlight_kinds(SAMPLE_SQL);
        assert!(kinds.len() >= 3, "{kinds:?}");
    }
}
