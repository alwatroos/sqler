// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

use crate::Value;

/// One statement and its positional parameters.
///
/// `Debug` prints only the length of the SQL text and the number of parameters: the parameters
/// are user data and the text may hold literals, and a `Query` ends up in logs and panic
/// messages.
#[derive(Clone, PartialEq)]
#[non_exhaustive]
pub struct Query {
    /// The SQL text. One statement per query.
    pub sql: String,
    /// Positional parameters, bound in order.
    pub params: Vec<Value>,
}

impl std::fmt::Debug for Query {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Query")
            .field("sql_len", &self.sql.len())
            .field("params", &self.params.len())
            .finish()
    }
}

impl Query {
    /// Creates a query without parameters.
    #[must_use]
    pub fn new(sql: impl Into<String>) -> Self {
        Self {
            sql: sql.into(),
            params: Vec::new(),
        }
    }

    /// Sets the positional parameters.
    #[must_use]
    pub fn with_params(mut self, params: Vec<Value>) -> Self {
        self.params = params;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_has_no_params() {
        let q = Query::new("select 1");
        assert_eq!(q.sql, "select 1");
        assert_eq!(q.params.len(), 0);
    }

    #[test]
    fn debug_prints_counts_only() {
        let q = Query::new("select 'sekrit-sql' from t where a = ?").with_params(vec![
            Value::Text("sekrit-value".into()),
            Value::Int(424_242),
        ]);
        let shown = format!("{q:?}");
        assert!(!shown.contains("sekrit"), "{shown}");
        assert!(!shown.contains("424242"), "{shown}");
        assert!(!shown.contains("select"), "{shown}");
        assert!(shown.contains("params: 2"), "{shown}");
    }

    #[test]
    fn with_params_sets_in_order() {
        let q = Query::new("select ?, ?").with_params(vec![Value::Int(1), Value::Null]);
        assert_eq!(q.params, vec![Value::Int(1), Value::Null]);
    }
}
