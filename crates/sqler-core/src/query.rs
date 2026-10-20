// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

use crate::Value;

/// One statement and its positional parameters.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Query {
    /// The SQL text. One statement per query.
    pub sql: String,
    /// Positional parameters, bound in order.
    pub params: Vec<Value>,
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
    fn with_params_sets_in_order() {
        let q = Query::new("select ?, ?").with_params(vec![Value::Int(1), Value::Null]);
        assert_eq!(q.params, vec![Value::Int(1), Value::Null]);
    }
}
