// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Breadcrumbs: where a tab's content lives (connection, database, object).

use sqler_driver_api::sqler_core::{DbObject, Environment};

/// The path segments of a tab and the environment of its connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Breadcrumbs {
    /// The segments, outermost first.
    pub items: Vec<String>,
    /// The environment of the connection.
    pub env: Option<Environment>,
}

impl Breadcrumbs {
    /// The segments joined with ` › `.
    #[must_use]
    pub fn label(&self) -> String {
        self.items.join(" › ")
    }
}

/// The segments of an object: the schema (else the database), the owning table, the name.
#[must_use]
pub fn object_segments(object: &DbObject) -> Vec<String> {
    let p = &object.parent;
    [
        p.schema.as_ref().or(p.database.as_ref()),
        p.table.as_ref(),
        Some(&object.name),
    ]
    .into_iter()
    .flatten()
    .cloned()
    .collect()
}

#[cfg(test)]
mod tests {
    use sqler_driver_api::sqler_core::{ObjectKind, ObjectPath};

    use super::*;

    #[test]
    fn schema_table() {
        let path = ObjectPath::root().in_database("app").in_schema("public");
        let o = DbObject::new(ObjectKind::Table, path, "users");
        assert_eq!(object_segments(&o), ["public", "users"]);
    }

    #[test]
    fn mysql_database_table() {
        let path = ObjectPath::root().in_database("shop");
        let o = DbObject::new(ObjectKind::Table, path, "orders");
        assert_eq!(object_segments(&o), ["shop", "orders"]);
    }

    #[test]
    fn index_with_its_table() {
        let path = ObjectPath::root().in_schema("main").in_table("users");
        let o = DbObject::new(ObjectKind::Index, path, "users_ix");
        assert_eq!(object_segments(&o), ["main", "users", "users_ix"]);
        let bare = DbObject::new(ObjectKind::Table, ObjectPath::root(), "t");
        assert_eq!(object_segments(&bare), ["t"]);
    }

    #[test]
    fn label_joins() {
        let c = Breadcrumbs {
            items: vec!["dev".to_owned(), "memory".to_owned()],
            env: None,
        };
        assert_eq!(c.label(), "dev › memory");
    }
}
