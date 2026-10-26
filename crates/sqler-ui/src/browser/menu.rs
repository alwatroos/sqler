// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The context menu of a tree node: which entries apply to which node.

use sqler_sql::{Dialect, Script, supports};

use super::tree::TreeNodeKind;

/// One entry of the object context menu. Each one is also a command (`object:...`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum MenuEntry {
    /// Open the rows of the object in a grid tab (`SELECT *` with a limit).
    OpenData,
    /// Open a SQL editor that starts with the `SELECT *` of the object.
    OpenInEditor,
    /// Open a SQL editor with the `CREATE` script.
    GenerateCreate,
    /// Open a SQL editor with the `SELECT *` script.
    GenerateSelect,
    /// Open a SQL editor with the `INSERT` template.
    GenerateInsert,
    /// Open a SQL editor with the `DROP` script (nothing runs).
    GenerateDrop,
    /// Drop the object after a confirmation.
    Drop,
    /// Copy the quoted name to the clipboard.
    CopyName,
    /// Re-fetch the node and everything below it.
    Refresh,
    /// Show the properties of the object.
    Properties,
}

impl MenuEntry {
    /// The label in the context menu.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::OpenData => "Open Data",
            Self::OpenInEditor => "Open in SQL Editor",
            Self::GenerateCreate => "Generate CREATE",
            Self::GenerateSelect => "Generate SELECT",
            Self::GenerateInsert => "Generate INSERT",
            Self::GenerateDrop => "Generate DROP",
            Self::Drop => "Drop...",
            Self::CopyName => "Copy Name",
            Self::Refresh => "Refresh",
            Self::Properties => "Properties",
        }
    }

    /// Whether a separator goes before this entry in the menu: the generated scripts, the
    /// destructive entry and the clipboard entry each start a group.
    #[must_use]
    pub fn starts_group(self) -> bool {
        matches!(self, Self::GenerateCreate | Self::Drop | Self::CopyName)
    }
}

/// The entries for `node`, in menu order.
///
/// Script entries follow [`supports`]; a driver without a [`Dialect`] has no scripts, open data
/// or copy name (names need the dialect's quoting).
#[must_use]
pub fn menu_for(node: &TreeNodeKind, dialect: Option<Dialect>) -> Vec<MenuEntry> {
    match node {
        TreeNodeKind::Root | TreeNodeKind::Folder(_) | TreeNodeKind::Columns(_) => {
            vec![MenuEntry::Refresh]
        }
        TreeNodeKind::Object(obj) => {
            let mut out = Vec::new();
            if let Some(d) = dialect {
                let has = |script| supports(d, obj.kind, script);
                let scripts = [
                    (MenuEntry::OpenData, Script::SelectStar),
                    (MenuEntry::OpenInEditor, Script::SelectStar),
                    (MenuEntry::GenerateCreate, Script::Create),
                    (MenuEntry::GenerateSelect, Script::SelectStar),
                    (MenuEntry::GenerateInsert, Script::InsertTemplate),
                    (MenuEntry::GenerateDrop, Script::Drop),
                    (MenuEntry::Drop, Script::Drop),
                ];
                out.extend(scripts.iter().filter(|(_, s)| has(*s)).map(|(e, _)| *e));
                out.push(MenuEntry::CopyName);
            }
            out.push(MenuEntry::Refresh);
            out.push(MenuEntry::Properties);
            out
        }
        TreeNodeKind::Column { .. } => {
            let mut out = Vec::new();
            if dialect.is_some() {
                out.push(MenuEntry::CopyName);
            }
            out.push(MenuEntry::Properties);
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use sqler_catalog::NodeKey;
    use sqler_driver_api::sqler_core::{ColumnInfo, DataType, DbObject, ObjectKind, ObjectPath};

    use super::*;

    fn object(kind: ObjectKind) -> TreeNodeKind {
        TreeNodeKind::Object(DbObject::new(
            kind,
            ObjectPath::root().in_schema("public").in_table("t"),
            "x",
        ))
    }

    #[test]
    fn starts_group_marks_the_group_leaders() {
        let leaders = [
            MenuEntry::GenerateCreate,
            MenuEntry::Drop,
            MenuEntry::CopyName,
        ];
        for entry in [
            MenuEntry::OpenData,
            MenuEntry::OpenInEditor,
            MenuEntry::GenerateCreate,
            MenuEntry::GenerateSelect,
            MenuEntry::GenerateInsert,
            MenuEntry::GenerateDrop,
            MenuEntry::Drop,
            MenuEntry::CopyName,
            MenuEntry::Refresh,
            MenuEntry::Properties,
        ] {
            assert_eq!(entry.starts_group(), leaders.contains(&entry), "{entry:?}");
        }
    }

    #[test]
    fn root_and_folders_only_refresh() {
        let folder = TreeNodeKind::Folder(NodeKey::new(ObjectPath::root(), ObjectKind::Table));
        for node in [TreeNodeKind::Root, folder] {
            assert_eq!(
                menu_for(&node, Some(Dialect::Postgres)),
                vec![MenuEntry::Refresh]
            );
            assert_eq!(menu_for(&node, None), vec![MenuEntry::Refresh]);
        }
    }

    #[test]
    fn a_table_gets_every_entry() {
        assert_eq!(
            menu_for(&object(ObjectKind::Table), Some(Dialect::Postgres)),
            vec![
                MenuEntry::OpenData,
                MenuEntry::OpenInEditor,
                MenuEntry::GenerateCreate,
                MenuEntry::GenerateSelect,
                MenuEntry::GenerateInsert,
                MenuEntry::GenerateDrop,
                MenuEntry::Drop,
                MenuEntry::CopyName,
                MenuEntry::Refresh,
                MenuEntry::Properties,
            ]
        );
    }

    #[test]
    fn entries_follow_supports() {
        for dialect in Dialect::ALL {
            for kind in ObjectKind::ALL {
                let menu = menu_for(&object(kind), Some(dialect));
                assert_eq!(
                    menu.contains(&MenuEntry::OpenData),
                    supports(dialect, kind, Script::SelectStar)
                );
                assert_eq!(
                    menu.contains(&MenuEntry::OpenInEditor),
                    supports(dialect, kind, Script::SelectStar)
                );
                assert_eq!(
                    menu.contains(&MenuEntry::GenerateCreate),
                    supports(dialect, kind, Script::Create)
                );
                assert_eq!(
                    menu.contains(&MenuEntry::GenerateInsert),
                    supports(dialect, kind, Script::InsertTemplate)
                );
                assert_eq!(
                    menu.contains(&MenuEntry::Drop),
                    supports(dialect, kind, Script::Drop)
                );
                assert_eq!(
                    menu.contains(&MenuEntry::GenerateDrop),
                    menu.contains(&MenuEntry::Drop)
                );
                assert!(menu.contains(&MenuEntry::CopyName));
                assert!(menu.contains(&MenuEntry::Refresh));
                assert!(menu.contains(&MenuEntry::Properties));
            }
        }
    }

    #[test]
    fn a_function_has_no_open_data_or_insert() {
        let menu = menu_for(&object(ObjectKind::Function), Some(Dialect::Postgres));
        assert!(!menu.contains(&MenuEntry::OpenData));
        assert!(!menu.contains(&MenuEntry::OpenInEditor));
        assert!(!menu.contains(&MenuEntry::GenerateInsert));
        assert!(menu.contains(&MenuEntry::Drop));
    }

    #[test]
    fn without_a_dialect_only_refresh_and_properties() {
        assert_eq!(
            menu_for(&object(ObjectKind::Table), None),
            vec![MenuEntry::Refresh, MenuEntry::Properties]
        );
    }

    #[test]
    fn a_column_copies_its_name_and_shows_properties() {
        let column = TreeNodeKind::Column {
            table: DbObject::new(ObjectKind::Table, ObjectPath::root(), "t"),
            column: ColumnInfo::new("id", DataType::Integer),
        };
        assert_eq!(
            menu_for(&column, Some(Dialect::Sqlite)),
            vec![MenuEntry::CopyName, MenuEntry::Properties]
        );
        assert_eq!(menu_for(&column, None), vec![MenuEntry::Properties]);
    }

    #[test]
    fn labels_are_unique() {
        let all = [
            MenuEntry::OpenData,
            MenuEntry::OpenInEditor,
            MenuEntry::GenerateCreate,
            MenuEntry::GenerateSelect,
            MenuEntry::GenerateInsert,
            MenuEntry::GenerateDrop,
            MenuEntry::Drop,
            MenuEntry::CopyName,
            MenuEntry::Refresh,
            MenuEntry::Properties,
        ];
        let labels: std::collections::HashSet<_> = all.iter().map(|e| e.label()).collect();
        assert_eq!(labels.len(), all.len());
    }
}
