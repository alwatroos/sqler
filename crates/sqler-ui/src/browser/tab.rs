// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The content of the read-only DDL and properties tabs.

use sqler_driver_api::CancelFlag;
use sqler_driver_api::sqler_core::{ColumnInfo, DbObject, Table};

/// One line of the properties view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Property {
    /// The name of the field.
    pub label: String,
    /// Its value.
    pub value: String,
}

impl Property {
    fn new(label: &str, value: impl Into<String>) -> Self {
        Self {
            label: label.to_owned(),
            value: value.into(),
        }
    }
}

/// What an object tab shows.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ObjectTabContent {
    /// The script or properties are being fetched.
    Loading,
    /// A generated script.
    Script(String),
    /// The fields of an object.
    Properties(Vec<Property>),
    /// The job failed.
    Failed(String),
}

/// A read-only tab about one object.
#[derive(Debug, Clone)]
pub struct ObjectTab {
    /// The tab title.
    pub title: String,
    /// What it shows.
    pub content: ObjectTabContent,
    /// Cancels the job that fills the tab; set when the tab closes.
    pub cancel: CancelFlag,
}

impl ObjectTab {
    /// A tab in the loading state.
    #[must_use]
    pub fn loading(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            content: ObjectTabContent::Loading,
            cancel: CancelFlag::new(),
        }
    }

    /// The text the Copy button puts on the clipboard: the script, or `label: value` lines.
    #[must_use]
    pub fn copy_text(&self) -> Option<String> {
        match &self.content {
            ObjectTabContent::Script(text) => Some(text.clone()),
            ObjectTabContent::Properties(props) => Some(
                props
                    .iter()
                    .map(|p| format!("{}: {}", p.label, p.value))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            _ => None,
        }
    }
}

fn path_part(label: &str, value: Option<&String>, out: &mut Vec<Property>) {
    if let Some(value) = value {
        out.push(Property::new(label, value.clone()));
    }
}

/// The fields of an object; with the model of a table, view or materialized view also its
/// columns and keys.
#[must_use]
pub fn properties(obj: &DbObject, table: Option<&Table>) -> Vec<Property> {
    let mut out = vec![
        Property::new("Name", obj.name.clone()),
        Property::new("Kind", obj.kind.as_str()),
    ];
    path_part("Database", obj.parent.database.as_ref(), &mut out);
    path_part("Schema", obj.parent.schema.as_ref(), &mut out);
    path_part("Table", obj.parent.table.as_ref(), &mut out);
    if let Some(table) = table {
        out.push(Property::new("Columns", table.columns.len().to_string()));
        if let Some(pk) = &table.primary_key {
            out.push(Property::new("Primary key", pk.columns.join(", ")));
        }
        out.push(Property::new(
            "Foreign keys",
            table.foreign_keys.len().to_string(),
        ));
        for fk in &table.foreign_keys {
            let target = match &fk.referenced_schema {
                Some(schema) => format!("{schema}.{}", fk.referenced_table),
                None => fk.referenced_table.clone(),
            };
            out.push(Property::new(
                "Foreign key",
                format!(
                    "({}) -> {target} ({})",
                    fk.columns.join(", "),
                    fk.referenced_columns.join(", ")
                ),
            ));
        }
    }
    out
}

/// The fields of one column.
#[must_use]
pub fn column_properties(table: &DbObject, column: &ColumnInfo) -> Vec<Property> {
    let mut out = vec![
        Property::new("Name", column.name.clone()),
        Property::new("Table", table.name.clone()),
        Property::new("Type", format!("{:?}", column.data_type).to_lowercase()),
    ];
    if let Some(native) = &column.native_type {
        out.push(Property::new("Native type", native.clone()));
    }
    out.push(Property::new(
        "Nullable",
        if column.nullable { "yes" } else { "no" },
    ));
    if let Some(default) = &column.default {
        out.push(Property::new("Default", default.clone()));
    }
    out
}

#[cfg(test)]
mod tests {
    use sqler_driver_api::sqler_core::{DataType, ForeignKey, ObjectKind, ObjectPath, PrimaryKey};

    use super::*;

    fn obj() -> DbObject {
        DbObject::new(
            ObjectKind::Table,
            ObjectPath::root().in_schema("public"),
            "t",
        )
    }

    fn table() -> Table {
        let mut t = Table::new(obj());
        t.columns = vec![
            ColumnInfo::new("id", DataType::Integer).with_nullable(false),
            ColumnInfo::new("owner", DataType::Integer),
        ];
        t.primary_key = Some(PrimaryKey::new(vec!["id".into()]));
        t.foreign_keys = vec![
            ForeignKey::new(vec!["owner".into()], "users", vec!["id".into()])
                .with_referenced_schema("auth"),
        ];
        t
    }

    #[test]
    fn object_properties() {
        let props = properties(&obj(), None);
        let text: Vec<_> = props
            .iter()
            .map(|p| (p.label.as_str(), p.value.as_str()))
            .collect();
        assert_eq!(
            text,
            vec![("Name", "t"), ("Kind", "table"), ("Schema", "public")]
        );
    }

    #[test]
    fn table_properties_list_columns_and_keys() {
        let props = properties(&obj(), Some(&table()));
        let get = |l: &str| props.iter().find(|p| p.label == l).map(|p| p.value.clone());
        assert_eq!(get("Columns").as_deref(), Some("2"));
        assert_eq!(get("Primary key").as_deref(), Some("id"));
        assert_eq!(get("Foreign keys").as_deref(), Some("1"));
        assert_eq!(
            get("Foreign key").as_deref(),
            Some("(owner) -> auth.users (id)")
        );
    }

    #[test]
    fn column_fields() {
        let c = ColumnInfo::new("n", DataType::Text)
            .with_native_type("varchar(9)")
            .with_default("'x'");
        let props = column_properties(&obj(), &c);
        let text: Vec<_> = props
            .iter()
            .map(|p| format!("{}={}", p.label, p.value))
            .collect();
        assert_eq!(
            text,
            vec![
                "Name=n",
                "Table=t",
                "Type=text",
                "Native type=varchar(9)",
                "Nullable=yes",
                "Default='x'"
            ]
        );
    }

    #[test]
    fn copy_text_by_content() {
        let mut tab = ObjectTab::loading("DDL: t");
        assert_eq!(tab.copy_text(), None);
        tab.content = ObjectTabContent::Failed("x".into());
        assert_eq!(tab.copy_text(), None);
        tab.content = ObjectTabContent::Script("CREATE TABLE t ();\n".into());
        assert_eq!(tab.copy_text().as_deref(), Some("CREATE TABLE t ();\n"));
        tab.content = ObjectTabContent::Properties(properties(&obj(), None));
        assert_eq!(
            tab.copy_text().as_deref(),
            Some("Name: t\nKind: table\nSchema: public")
        );
        assert!(!tab.cancel.is_cancelled());
    }
}
