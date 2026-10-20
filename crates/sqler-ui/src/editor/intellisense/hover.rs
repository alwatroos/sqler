// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Hover text and diagnostic codes, as plain data.

use std::fmt::Write as _;
use std::ops::Range;

use sqler_completion::{
    DiagnosticCode, HoverRequest, HoverTarget, RelationInfo, RelationKind, hover,
};
use sqler_driver_api::sqler_core::DataType;
use sqler_sql::Dialect;

use super::schema::CatalogSchema;

fn kind_words(kind: RelationKind) -> &'static str {
    match kind {
        RelationKind::Table => "table",
        RelationKind::View => "view",
        RelationKind::MaterializedView => "materialized view",
        RelationKind::Cte => "common table expression",
        RelationKind::Subquery => "subquery",
        _ => "relation",
    }
}

fn qualified(schema: Option<&str>, name: &str) -> String {
    schema.map_or_else(|| name.to_owned(), |s| format!("{s}.{name}"))
}

fn relation_name(info: &RelationInfo) -> String {
    qualified(info.schema.as_deref(), &info.name)
}

fn capitalised(words: &str) -> String {
    let mut chars = words.chars();
    chars
        .next()
        .map_or_else(String::new, |c| c.to_uppercase().chain(chars).collect())
}

/// The Markdown for the identifier at the byte `offset`, with the byte range it covers.
///
/// - A column: its name, type and `NOT NULL`, the relation it belongs to, and the foreign key target.
/// - A relation: its qualified name and what kind it is.
/// - A column of a subquery or CTE: its name and the relation.
///
/// `None` when nothing known is under the cursor.
#[must_use]
pub fn hover_markdown(
    text: &str,
    offset: usize,
    dialect: Dialect,
    schema: &CatalogSchema,
) -> Option<(Range<usize>, String)> {
    let found = hover(&HoverRequest::new(text, offset, dialect), schema)?;
    let markdown = match found.target {
        HoverTarget::Column {
            relation,
            info,
            references,
        } => {
            let type_name = info
                .native_type
                .clone()
                .unwrap_or_else(|| data_type_name(info.data_type).to_owned());
            let not_null = if info.nullable { "" } else { " NOT NULL" };
            let mut out = format!(
                "**{}** `{type_name}`{not_null}\n\nColumn of {} `{}`",
                info.name,
                kind_words(relation.kind),
                relation_name(&relation),
            );
            if let Some(fk) = references {
                let _ = write!(
                    out,
                    "\n\nReferences `{}({})`",
                    qualified(fk.referenced_schema.as_deref(), &fk.referenced_table),
                    fk.referenced_columns.join(", "),
                );
            }
            out
        }
        HoverTarget::Relation(relation) => format!(
            "**{}**\n\n{}",
            relation_name(&relation),
            capitalised(kind_words(relation.kind)),
        ),
        HoverTarget::DerivedColumn { relation, name } => {
            format!("**{name}**\n\nColumn of `{}`", relation_name(&relation))
        }
        _ => return None,
    };
    Some((found.range, markdown))
}

/// The short name of a diagnostic code, for the `code` of the editor's diagnostic.
///
/// Codes added later read as `sql`.
/// The name shown for a column type that has no native type name.
#[must_use]
pub fn data_type_name(data_type: DataType) -> &'static str {
    match data_type {
        DataType::Boolean => "boolean",
        DataType::Integer => "integer",
        DataType::Float => "float",
        DataType::Decimal => "decimal",
        DataType::Text => "text",
        DataType::Binary => "binary",
        DataType::Date => "date",
        DataType::Time => "time",
        DataType::Timestamp => "timestamp",
        DataType::Json => "json",
        _ => "other",
    }
}

#[must_use]
pub fn code_name(code: DiagnosticCode) -> &'static str {
    match code {
        DiagnosticCode::UnknownSchema => "unknown-schema",
        DiagnosticCode::UnknownRelation => "unknown-relation",
        DiagnosticCode::UnknownColumn => "unknown-column",
        DiagnosticCode::UnknownQualifier => "unknown-qualifier",
        DiagnosticCode::AmbiguousColumn => "ambiguous-column",
        _ => "sql",
    }
}

#[cfg(test)]
mod tests {
    use sqler_catalog::CacheKey;

    use super::super::keys::{bootstrap_keys, load, prefetch_keys};
    use super::super::synthetic::synthetic_catalog;
    use super::*;

    fn loaded(text: &str) -> CatalogSchema {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .build()
            .expect("runtime");
        let (catalog, worker) = synthetic_catalog(5, 3);
        rt.spawn(worker.run());
        rt.block_on(load(&catalog, bootstrap_keys()));
        let probe = CatalogSchema::new(catalog.snapshot(), Dialect::Postgres);
        let keys: Vec<CacheKey> = prefetch_keys(text, Dialect::Postgres, &probe, 64);
        rt.block_on(load(&catalog, keys));
        CatalogSchema::new(catalog.snapshot(), Dialect::Postgres)
    }

    fn at(text: &str, needle: &str) -> usize {
        text.find(needle).expect("needle") + 1
    }

    #[test]
    fn a_column_shows_type_nullability_and_foreign_key() {
        let text = "SELECT u.c_1, u.id FROM t_00003 u";
        let schema = loaded(text);
        let (range, md) =
            hover_markdown(text, at(text, "c_1"), Dialect::Postgres, &schema).expect("hover");
        assert_eq!(&text[range], "c_1");
        assert_eq!(
            md,
            "**c_1** `text`\n\nColumn of table `public.t_00003`\n\nReferences `t_00002(id)`"
        );
        let (_, md) =
            hover_markdown(text, at(text, "id"), Dialect::Postgres, &schema).expect("hover");
        assert_eq!(
            md,
            "**id** `integer` NOT NULL\n\nColumn of table `public.t_00003`"
        );
    }

    #[test]
    fn a_relation_shows_its_name_and_kind() {
        let text = "SELECT 1 FROM t_00003 u";
        let schema = loaded(text);
        let (range, md) =
            hover_markdown(text, at(text, "t_00003"), Dialect::Postgres, &schema).expect("hover");
        assert_eq!(&text[range], "t_00003");
        assert_eq!(md, "**public.t_00003**\n\nTable");
    }

    #[test]
    fn a_derived_column_names_its_relation() {
        let text = "SELECT s.n FROM (SELECT 1 AS n) s";
        let schema = loaded(text);
        let (_, md) =
            hover_markdown(text, at(text, "s.n") + 2, Dialect::Postgres, &schema).expect("hover");
        assert!(md.starts_with("**n**\n\nColumn of `"), "{md}");
    }

    #[test]
    fn nothing_known_gives_nothing() {
        let text = "SELECT 1 FROM nope";
        let schema = loaded(text);
        assert!(hover_markdown(text, at(text, "nope"), Dialect::Postgres, &schema).is_none());
        assert!(hover_markdown(text, 0, Dialect::Postgres, &schema).is_none());
    }

    #[test]
    fn type_names_are_explicit() {
        assert_eq!(data_type_name(DataType::Timestamp), "timestamp");
        assert_eq!(data_type_name(DataType::Json), "json");
        assert_eq!(data_type_name(DataType::Decimal), "decimal");
        assert_eq!(data_type_name(DataType::Other), "other");
    }

    #[test]
    fn codes_have_stable_names() {
        assert_eq!(code_name(DiagnosticCode::UnknownSchema), "unknown-schema");
        assert_eq!(
            code_name(DiagnosticCode::UnknownRelation),
            "unknown-relation"
        );
        assert_eq!(code_name(DiagnosticCode::UnknownColumn), "unknown-column");
        assert_eq!(
            code_name(DiagnosticCode::UnknownQualifier),
            "unknown-qualifier"
        );
        assert_eq!(
            code_name(DiagnosticCode::AmbiguousColumn),
            "ambiguous-column"
        );
    }
}
