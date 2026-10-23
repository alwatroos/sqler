// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Columns and rows on their way to an exporter plugin.

use sqler_driver_api::sqler_core::{Column, Row};

use crate::query::{data_type_to_wit, type_name, value_to_wit};
use crate::runtime::wit::types as wit;

/// The columns as the guest sees them: name, logical type and the type name (the native one when known).
pub(crate) fn columns_to_wit(columns: &[Column]) -> Vec<wit::Column> {
    columns
        .iter()
        .map(|c| wit::Column {
            name: c.name.clone(),
            data_type: data_type_to_wit(c.data_type),
            type_name: type_name(c.native_type.as_deref(), c.data_type),
        })
        .collect()
}

/// The rows of one batch.
pub(crate) fn rows_to_wit(rows: &[Row]) -> Vec<Vec<wit::Value>> {
    rows.iter()
        .map(|row| row.0.iter().map(value_to_wit).collect())
        .collect()
}

#[cfg(test)]
mod tests {
    use sqler_driver_api::sqler_core::{DataType, Value};

    use super::*;

    #[test]
    fn columns_carry_name_type_and_type_name() {
        let mut native = Column::new("price", DataType::Decimal);
        native.native_type = Some("NUMERIC(10,2)".to_owned());
        let cols = columns_to_wit(&[Column::new("id", DataType::Integer), native]);
        assert_eq!(cols.len(), 2);
        assert_eq!(cols[0].name, "id");
        assert!(matches!(cols[0].data_type, wit::DataType::Integer));
        assert_eq!(cols[0].type_name, "integer");
        assert_eq!(cols[1].type_name, "NUMERIC(10,2)");
    }

    #[test]
    fn rows_keep_their_shape_and_nulls() {
        let rows = vec![
            Row(vec![Value::Int(1), Value::Text("a".into()), Value::Null]),
            Row(vec![Value::Int(2), Value::Null, Value::Bool(true)]),
        ];
        let out = rows_to_wit(&rows);
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|r| r.len() == 3));
        assert!(matches!(out[0][0], wit::Value::Integer(1)));
        assert!(matches!(&out[0][1], wit::Value::Text(t) if t == "a"));
        assert!(matches!(out[0][2], wit::Value::Null));
        assert!(matches!(out[1][2], wit::Value::Boolean(true)));
        assert!(rows_to_wit(&[]).is_empty());
    }
}
