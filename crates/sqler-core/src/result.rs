// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

use crate::{CoreError, DataType, Value};

/// Metadata of one result column.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Column {
    /// Column name.
    pub name: String,
    /// Logical type.
    pub data_type: DataType,
    /// The database's own type name, if known.
    pub native_type: Option<String>,
    /// Whether the column accepts `NULL`, if known.
    pub nullable: Option<bool>,
}

impl Column {
    /// Creates a column with unknown native type and nullability.
    #[must_use]
    pub fn new(name: impl Into<String>, data_type: DataType) -> Self {
        Self {
            name: name.into(),
            data_type,
            native_type: None,
            nullable: None,
        }
    }

    /// Sets the database's own type name.
    #[must_use]
    pub fn with_native_type(mut self, native_type: impl Into<String>) -> Self {
        self.native_type = Some(native_type.into());
        self
    }

    /// Sets the nullability.
    #[must_use]
    pub fn with_nullable(mut self, nullable: bool) -> Self {
        self.nullable = Some(nullable);
        self
    }
}

/// One row of values, in column order.
#[derive(Debug, Clone, PartialEq)]
pub struct Row(pub Vec<Value>);

impl Row {
    /// Number of values.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the row has no values.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The value at `index`, if any.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&Value> {
        self.0.get(index)
    }
}

/// A fully materialised result: columns, rows and the affected-row count.
#[derive(Debug, Clone, PartialEq)]
pub struct ResultSet {
    columns: Vec<Column>,
    rows: Vec<Row>,
    rows_affected: Option<u64>,
}

impl ResultSet {
    /// Creates an empty result with the given columns.
    #[must_use]
    pub fn new(columns: Vec<Column>) -> Self {
        Self {
            columns,
            rows: Vec::new(),
            rows_affected: None,
        }
    }

    /// Appends a row.
    ///
    /// # Errors
    /// Returns [`CoreError::RowWidth`] if the row width differs from the column count.
    pub fn push_row(&mut self, row: Row) -> Result<(), CoreError> {
        if row.len() != self.columns.len() {
            return Err(CoreError::RowWidth {
                expected: self.columns.len(),
                actual: row.len(),
            });
        }
        self.rows.push(row);
        Ok(())
    }

    /// The columns.
    #[must_use]
    pub fn columns(&self) -> &[Column] {
        &self.columns
    }

    /// The rows.
    #[must_use]
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Rows affected by a statement, if reported.
    #[must_use]
    pub fn rows_affected(&self) -> Option<u64> {
        self.rows_affected
    }

    /// Sets the affected-row count.
    pub fn set_rows_affected(&mut self, rows_affected: Option<u64>) {
        self.rows_affected = rows_affected;
    }

    /// Number of rows.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether there are no rows.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cols() -> Vec<Column> {
        vec![
            Column::new("a", DataType::Integer),
            Column::new("b", DataType::Text),
        ]
    }

    #[test]
    fn column_builders() {
        let c = Column::new("id", DataType::Integer);
        assert_eq!(c.name, "id");
        assert_eq!(c.data_type, DataType::Integer);
        assert_eq!(c.native_type, None);
        assert_eq!(c.nullable, None);
        let c = c.with_native_type("INTEGER").with_nullable(false);
        assert_eq!(c.native_type.as_deref(), Some("INTEGER"));
        assert_eq!(c.nullable, Some(false));
    }

    #[test]
    fn row_accessors() {
        let r = Row(vec![Value::Int(1), Value::Null]);
        assert_eq!(r.len(), 2);
        assert!(!r.is_empty());
        assert_eq!(r.get(0), Some(&Value::Int(1)));
        assert_eq!(r.get(2), None);
        assert!(Row(vec![]).is_empty());
    }

    #[test]
    fn push_row_ok() {
        let mut rs = ResultSet::new(cols());
        assert!(rs.is_empty());
        rs.push_row(Row(vec![Value::Int(1), Value::Text("x".into())]))
            .unwrap();
        assert_eq!(rs.len(), 1);
        assert!(!rs.is_empty());
        assert_eq!(rs.rows()[0].len(), 2);
        assert_eq!(rs.columns().len(), 2);
    }

    #[test]
    fn push_row_width_mismatch() {
        let mut rs = ResultSet::new(cols());
        let err = rs.push_row(Row(vec![Value::Null])).unwrap_err();
        assert_eq!(
            err,
            CoreError::RowWidth {
                expected: 2,
                actual: 1
            }
        );
        assert!(rs.is_empty());
    }

    #[test]
    fn rows_affected_round_trip() {
        let mut rs = ResultSet::new(vec![]);
        assert_eq!(rs.rows_affected(), None);
        rs.set_rows_affected(Some(3));
        assert_eq!(rs.rows_affected(), Some(3));
        rs.set_rows_affected(None);
        assert_eq!(rs.rows_affected(), None);
    }
}
