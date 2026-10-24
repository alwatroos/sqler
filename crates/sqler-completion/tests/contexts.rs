// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Snapshots of the top completion items for every context, in every dialect.

use std::fmt::Write as _;

use sqler_completion::{CompletionRequest, InsertFormat, MemorySchema, complete};
use sqler_core::{ColumnInfo, DataType, ObjectKind};
use sqler_sql::Dialect;

fn col(name: &str, ty: DataType, native: &str) -> ColumnInfo {
    ColumnInfo::new(name, ty).with_native_type(native)
}

/// The default schema of the dialect.
fn home(d: Dialect) -> &'static str {
    match d {
        Dialect::MySql => "shop",
        Dialect::Sqlite => "main",
        Dialect::TSql => "dbo",
        _ => "public",
    }
}

fn schema(d: Dialect) -> MemorySchema {
    let h = home(d);
    let mut s = MemorySchema::new([h]);
    s.add_table(
        h,
        "users",
        ObjectKind::Table,
        Some(vec![
            col("id", DataType::Integer, "integer"),
            ColumnInfo::new("name", DataType::Text),
            col("email", DataType::Text, "text"),
            col("created_at", DataType::Timestamp, "timestamp"),
        ]),
    );
    s.add_table(
        h,
        "orders",
        ObjectKind::Table,
        Some(vec![
            col("id", DataType::Integer, "integer"),
            col("user_id", DataType::Integer, "integer"),
            col("total", DataType::Decimal, "numeric"),
            col("status", DataType::Text, "text"),
        ]),
    );
    s.add_table(
        h,
        "order_items",
        ObjectKind::Table,
        Some(vec![
            col("order_id", DataType::Integer, "integer"),
            col("sku", DataType::Text, "text"),
            col("qty", DataType::Integer, "integer"),
        ]),
    );
    s.add_table(
        h,
        "Order Items",
        ObjectKind::Table,
        Some(vec![
            col("Item Id", DataType::Integer, "integer"),
            col("Label", DataType::Text, "text"),
        ]),
    );
    s.add_table(
        h,
        "active_users",
        ObjectKind::View,
        Some(vec![col("id", DataType::Integer, "integer")]),
    );
    s.add_table(h, "pending", ObjectKind::Table, None);
    s.add_table(
        "audit",
        "log",
        ObjectKind::Table,
        Some(vec![
            col("id", DataType::Integer, "integer"),
            col("message", DataType::Text, "text"),
        ]),
    );
    s
}

/// Runs `sql` (with `|` as the cursor, `{q}` as the dialect's identifier quote) in one dialect
/// and prints the top `top` items.
fn render_one(sql: &str, d: Dialect, s: &MemorySchema, top: usize) -> String {
    let quote = if d == Dialect::MySql { "`" } else { "\"" };
    let sql = sql.replace("{q}", quote);
    let Some(pos) = sql.find('|') else {
        panic!("no cursor marker in {sql}")
    };
    let text = sql.replacen('|', "", 1);
    let mut req = CompletionRequest::new(&text, pos, d);
    req.limit = 200;
    let done = complete(&req, s);
    let mut out = String::new();
    let _ = writeln!(
        out,
        "context: {:?}, replace: {:?} {:?}, items: {}",
        done.context,
        done.replace,
        &text[done.replace.clone()],
        done.items.len()
    );
    if !done.requests.is_empty() {
        let _ = writeln!(out, "requests: {:?}", done.requests);
    }
    for (i, item) in done.items.iter().take(top).enumerate() {
        let snippet = if item.format == InsertFormat::Snippet {
            " snippet"
        } else {
            ""
        };
        let detail = item.detail.as_deref().unwrap_or("-");
        let _ = writeln!(
            out,
            "{:>2}. {:?} [{:?}] insert={:?}{snippet} detail={detail:?}",
            i + 1,
            item.label,
            item.kind,
            item.insert_text,
        );
    }
    out
}

/// Renders the same SQL in every dialect.
fn render(sql: &str, top: usize) -> String {
    let mut out = format!("sql: {}\n", sql.replace("{q}", "\""));
    for d in Dialect::ALL {
        let _ = write!(out, "\n== {} ==\n", d.as_str());
        out.push_str(&render_one(sql, d, &schema(d), top));
    }
    out
}

macro_rules! snap {
    ($name:ident, $sql:expr) => {
        snap!($name, $sql, 6);
    };
    ($name:ident, $sql:expr, $top:expr) => {
        #[test]
        fn $name() {
            insta::assert_snapshot!(stringify!($name), render($sql, $top));
        }
    };
}

// AC1: contexts.
snap!(from_empty, "SELECT * FROM |", 8);
snap!(from_prefix, "SELECT * FROM us|");
snap!(from_fuzzy, "SELECT * FROM oi|");
snap!(join_relation, "SELECT * FROM users u JOIN |", 8);
snap!(comma_relation, "SELECT * FROM users u, ord|");
snap!(relation_tail, "SELECT * FROM users u |", 10);
snap!(
    select_list,
    "SELECT | FROM users u JOIN orders o ON o.user_id = u.id",
    12
);
snap!(select_list_prefix, "SELECT na| FROM users u");
snap!(where_clause, "SELECT * FROM users u WHERE |", 8);
snap!(
    on_clause,
    "SELECT * FROM users u JOIN orders o ON o.user_id = u.|"
);
snap!(
    on_clause_unqualified,
    "SELECT * FROM users u JOIN orders o ON |",
    12
);
snap!(
    group_by,
    "SELECT status, count(*) FROM orders GROUP BY |",
    8
);
snap!(order_by, "SELECT * FROM users ORDER BY cre|");
snap!(
    having,
    "SELECT status FROM orders GROUP BY status HAVING sum(|",
    8
);
snap!(schema_dot, "SELECT * FROM audit.|");
snap!(schema_dot_prefix, "SELECT * FROM audit.l|");
snap!(home_schema_dot, "SELECT * FROM public.|", 10);
snap!(schema_table_dot, "SELECT audit.log.| FROM audit.log");
snap!(alias_dot, "SELECT u.| FROM users u");
snap!(alias_dot_prefix, "SELECT u.em| FROM users u");
snap!(alias_dot_where, "SELECT * FROM users u WHERE u.|");
snap!(insert_columns, "INSERT INTO orders (|");
snap!(insert_columns_second, "INSERT INTO orders (id, st|");
snap!(update_set, "UPDATE users SET |", 8);
snap!(delete_where, "DELETE FROM orders WHERE |", 8);

// AC2: aliases, CTEs, subqueries, quoting, case.
snap!(alias_as, "SELECT u.| FROM users AS u");
snap!(alias_uppercase_keywords, "SELECT U.| FROM USERS U");
snap!(
    pg_uppercase_alias_and_ambiguous_column,
    "SELECT | FROM USERS U JOIN orders O ON true",
    12
);
snap!(
    pg_uppercase_cte_name,
    "WITH Recent AS (SELECT Total FROM orders) SELECT * FROM re|"
);
snap!(
    pg_uppercase_cte_columns,
    "WITH Recent AS (SELECT Total FROM orders) SELECT x.| FROM Recent x"
);
snap!(
    pg_quoted_cte_keeps_case,
    "WITH {q}Recent{q} AS (SELECT {q}Total{q} FROM orders) SELECT | FROM {q}Recent{q}"
);
snap!(
    alias_two_tables,
    "SELECT o.| FROM users u JOIN orders o ON true"
);
snap!(bare_table_name_qualifier, "SELECT orders.| FROM orders");
snap!(
    cte_dot,
    "WITH recent AS (SELECT id, total FROM orders) SELECT r.| FROM recent r"
);
snap!(
    cte_declared_columns,
    "WITH t (a, b) AS (SELECT 1, 2) SELECT t.| FROM t"
);
snap!(cte_in_from, "WITH recent AS (SELECT 1) SELECT * FROM re|");
snap!(
    subquery_alias_dot,
    "SELECT s.| FROM (SELECT id, name AS who, count(*) c FROM users u) s"
);
snap!(
    subquery_inner_scope,
    "SELECT * FROM users u WHERE EXISTS (SELECT 1 FROM orders o WHERE o.user_id = |)",
    12
);
snap!(quoted_alias_table, "SELECT oi.| FROM {q}Order Items{q} oi");
snap!(
    quoted_qualifier,
    "SELECT {q}oi{q}.| FROM {q}Order Items{q} {q}oi{q}"
);
snap!(quoted_table_listing, "SELECT * FROM ord|");
snap!(inside_open_quote, "SELECT * FROM {q}Order It|");
snap!(case_insensitive_table, "SELECT u.| FROM Users u");
snap!(cursor_before_from, "SELECT u.| FROM users u");
snap!(
    multi_statement,
    "SELECT 1; SELECT o.| FROM orders o; SELECT 3"
);

// Ambiguity and ranking.
snap!(
    ambiguous_columns,
    "SELECT id| FROM users u JOIN orders o ON true",
    8
);
snap!(
    rank_column_before_function_before_keyword,
    "SELECT * FROM users u WHERE na|",
    10
);
snap!(
    rank_prefix_before_fuzzy,
    "SELECT * FROM users u WHERE e|",
    10
);
snap!(rank_word_segment, "SELECT * FROM orders WHERE uid|");
snap!(
    rank_in_scope_columns_first,
    "SELECT * FROM orders WHERE |",
    10
);

// AC3: keywords, functions, snippets.
snap!(statement_start, "|", 12);
snap!(statement_start_prefix, "sel|");
snap!(snippet_upd, "upd|");
snap!(keyword_case_lower, "select * from orders wh|");
snap!(keyword_case_upper, "select * from orders WH|");
snap!(function_count, "SELECT cou| FROM orders", 4);
snap!(
    function_dates,
    "SELECT * FROM orders WHERE created_at > no|",
    6
);
snap!(function_json, "SELECT json| FROM orders", 8);
snap!(function_group_concat, "SELECT group| FROM orders", 4);

// AC4: not loaded.
snap!(not_loaded_columns, "SELECT p.| FROM pending p");
snap!(
    not_loaded_columns_expression,
    "SELECT | FROM pending p JOIN orders o ON true",
    6
);
snap!(not_loaded_insert_columns, "INSERT INTO pending (|");
snap!(unknown_table_no_request, "SELECT x.| FROM nothing_here x");

// Nothing to complete.
snap!(in_string, "SELECT 'ab|c' FROM orders");
snap!(in_line_comment, "SELECT 1 -- note|");
snap!(in_block_comment, "SELECT /* no|te */ 1");
snap!(after_as, "SELECT id AS |");
snap!(after_number, "SELECT 12|");

#[test]
fn relations_not_loaded_make_a_request() {
    let mut s = schema(Dialect::Postgres);
    s.mark_relations_not_loaded("public");
    let out = render_one("SELECT * FROM |", Dialect::Postgres, &s, 5);
    insta::assert_snapshot!("relations_not_loaded", out);
    let out = render_one("SELECT u.| FROM users u", Dialect::Postgres, &s, 5);
    insta::assert_snapshot!("relations_not_loaded_alias", out);
}

#[test]
fn search_path_order_and_multiple_schemas() {
    let mut s = MemorySchema::new(["tenant", "public"]);
    s.add_table(
        "public",
        "t",
        ObjectKind::Table,
        Some(vec![ColumnInfo::new("pub_col", DataType::Text)]),
    );
    s.add_table(
        "tenant",
        "t",
        ObjectKind::Table,
        Some(vec![ColumnInfo::new("tenant_col", DataType::Text)]),
    );
    let out = render_one("SELECT t.| FROM t", Dialect::Postgres, &s, 5);
    insta::assert_snapshot!("search_path_first_schema_wins", out);
}

#[test]
fn dialect_quoting_of_inserted_names() {
    let mut s = MemorySchema::new(["main"]);
    s.add_table(
        "main",
        "T",
        ObjectKind::Table,
        Some(vec![
            ColumnInfo::new("select", DataType::Text),
            ColumnInfo::new("MixedCase", DataType::Text),
            ColumnInfo::new("plain", DataType::Text),
            ColumnInfo::new("has space", DataType::Text),
        ]),
    );
    let mut out = String::new();
    for d in Dialect::ALL {
        let _ = write!(
            out,
            "== {} ==\n{}",
            d.as_str(),
            render_one("SELECT t.| FROM {q}T{q} t", d, &s, 5)
        );
    }
    insta::assert_snapshot!("quoting_per_dialect", out);
}
