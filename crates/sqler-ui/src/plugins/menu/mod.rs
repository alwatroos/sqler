// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The context a menu command of a plugin runs with (ADR-0060). The workspace reads what is selected and hands the
//! plain data here; each builder returns the [`CommandContext`] or the one-line notice for the user.

use sqler_driver_api::sqler_core::{Column, DbObject, ObjectKind};
use sqler_plugin_api::{ColumnRef, CommandContext, ObjectPathRef, ObjectRef};

/// The most bytes of editor selection a plugin receives.
pub const SELECTION_CAP: usize = 256 * 1024;
/// The notice when nothing is selected in the sidebar (the text of the built-in object commands).
pub const NO_OBJECT: &str = "Select an object in the sidebar first.";
/// The notice when no results grid is open.
pub const NO_RESULT: &str = "Open a results grid first.";
/// The notice when no SQL editor is open.
pub const NO_EDITOR: &str = "Open a SQL editor first.";

/// The object context: the connection and the selected object, if the kind is one the command applies to
/// (`kinds` empty = every kind).
///
/// # Errors
///
/// The notice: nothing is selected, or the object is of another kind.
pub fn object_context(
    connection: Option<&str>,
    object: Option<&DbObject>,
    kinds: &[ObjectKind],
) -> Result<CommandContext, String> {
    let (Some(connection), Some(object)) = (connection, object) else {
        return Err(NO_OBJECT.to_owned());
    };
    if !kinds.is_empty() && !kinds.contains(&object.kind) {
        return Err(format!(
            "This command does not apply to a {}.",
            kind_label(object.kind)
        ));
    }
    Ok(CommandContext::Object {
        connection: connection.to_owned(),
        object: ObjectRef {
            kind: object.kind,
            parent: ObjectPathRef {
                database: object.parent.database.clone().unwrap_or_default(),
                schema: object.parent.schema.clone().unwrap_or_default(),
                table: object.parent.table.clone(),
            },
            name: object.name.clone(),
        },
    })
}

/// The editor context. `editor` is `None` without an active SQL editor; otherwise the connection of its tab, if any,
/// and the selected text (`None` when nothing is selected).
///
/// # Errors
///
/// The notice: there is no SQL editor.
pub fn editor_context(
    editor: Option<(Option<&str>, Option<&str>)>,
) -> Result<CommandContext, String> {
    let Some((connection, selection)) = editor else {
        return Err(NO_EDITOR.to_owned());
    };
    Ok(CommandContext::Editor {
        connection: connection.map(str::to_owned),
        selection: cap_selection(selection.unwrap_or_default()),
    })
}

/// The results context: the connection and the columns of the active result.
///
/// # Errors
///
/// The notice: there is no results grid.
pub fn results_context(
    connection: Option<&str>,
    columns: Option<&[Column]>,
) -> Result<CommandContext, String> {
    let (Some(connection), Some(columns)) = (connection, columns) else {
        return Err(NO_RESULT.to_owned());
    };
    Ok(CommandContext::Results {
        connection: connection.to_owned(),
        columns: columns
            .iter()
            .map(|c| ColumnRef {
                name: c.name.clone(),
                data_type: c.data_type,
                type_name: c
                    .native_type
                    .clone()
                    .unwrap_or_else(|| format!("{:?}", c.data_type).to_ascii_lowercase()),
            })
            .collect(),
    })
}

/// `text` cut to [`SELECTION_CAP`] bytes at a character boundary.
#[must_use]
pub fn cap_selection(text: &str) -> String {
    if text.len() <= SELECTION_CAP {
        return text.to_owned();
    }
    let mut end = SELECTION_CAP;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

/// The lowercase word for a kind in a notice.
fn kind_label(kind: ObjectKind) -> String {
    kind.as_str().replace('-', " ")
}

#[cfg(test)]
mod tests {
    use sqler_driver_api::sqler_core::{DataType, ObjectPath};

    use super::*;

    fn table() -> DbObject {
        DbObject::new(
            ObjectKind::Table,
            ObjectPath::root().in_database("shop").in_schema("public"),
            "orders",
        )
    }

    #[test]
    fn object_without_selection_gives_the_existing_notice() {
        let err = object_context(None, None, &[]).unwrap_err();
        assert_eq!(err, "Select an object in the sidebar first.");
        assert_eq!(
            object_context(Some("c1"), None, &[]).unwrap_err(),
            NO_OBJECT
        );
    }

    #[test]
    fn object_context_carries_the_path_and_checks_the_kind() {
        let obj = table();
        let ctx = object_context(Some("c1"), Some(&obj), &[]).unwrap();
        let CommandContext::Object { connection, object } = ctx else {
            panic!("expected an object context");
        };
        assert_eq!(connection, "c1");
        assert_eq!(object.kind, ObjectKind::Table);
        assert_eq!(object.name, "orders");
        assert_eq!(object.parent.database, "shop");
        assert_eq!(object.parent.schema, "public");
        assert_eq!(object.parent.table, None);
        assert!(
            object_context(
                Some("c1"),
                Some(&obj),
                &[ObjectKind::Table, ObjectKind::View]
            )
            .is_ok()
        );
        let err = object_context(Some("c1"), Some(&obj), &[ObjectKind::View]).unwrap_err();
        assert_eq!(err, "This command does not apply to a table.");
        let mat = DbObject::new(ObjectKind::MaterializedView, ObjectPath::root(), "m");
        let err = object_context(Some("c1"), Some(&mat), &[ObjectKind::Table]).unwrap_err();
        assert_eq!(err, "This command does not apply to a materialized view.");
    }

    #[test]
    fn editor_context_needs_an_editor() {
        assert_eq!(editor_context(None).unwrap_err(), NO_EDITOR);
        assert_eq!(
            editor_context(Some((Some("c1"), Some("select 1")))).unwrap(),
            CommandContext::Editor {
                connection: Some("c1".into()),
                selection: "select 1".into()
            }
        );
        assert_eq!(
            editor_context(Some((None, None))).unwrap(),
            CommandContext::Editor {
                connection: None,
                selection: String::new()
            }
        );
    }

    #[test]
    fn a_selection_is_capped_on_a_character_boundary() {
        let big = "é".repeat(SELECTION_CAP);
        let capped = cap_selection(&big);
        assert!(capped.len() <= SELECTION_CAP);
        assert!(capped.len() > SELECTION_CAP - 4);
        assert!(big.starts_with(&capped));
        assert_eq!(cap_selection("small"), "small");
        let exact = "a".repeat(SELECTION_CAP);
        assert_eq!(cap_selection(&exact).len(), SELECTION_CAP);
    }

    #[test]
    fn results_context_needs_a_grid() {
        assert_eq!(
            results_context(None, None).unwrap_err(),
            "Open a results grid first."
        );
        assert_eq!(results_context(Some("c1"), None).unwrap_err(), NO_RESULT);
        let mut native = Column::new("id", DataType::Integer);
        native.native_type = Some("int8".into());
        let cols = [native, Column::new("note", DataType::Text)];
        let CommandContext::Results {
            connection,
            columns,
        } = results_context(Some("c1"), Some(&cols)).unwrap()
        else {
            panic!("expected a results context");
        };
        assert_eq!(connection, "c1");
        assert_eq!(columns[0].type_name, "int8");
        assert_eq!(columns[1].type_name, "text");
        assert_eq!(columns[1].data_type, DataType::Text);
    }
}
