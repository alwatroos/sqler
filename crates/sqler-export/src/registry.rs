// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The format registry.

use crate::{CsvFormat, ExportError, Format, JsonFormat, MarkdownFormat, SqlFormat};

/// The id of the `CSV` format.
pub const CSV: &str = "csv";
/// The id of the `JSON` format.
pub const JSON: &str = "json";
/// The id of the `SQL` `INSERT` format.
pub const SQL: &str = "sql";
/// The id of the Markdown table format.
pub const MARKDOWN: &str = "markdown";

/// A list of formats by id, in registration order.
#[derive(Default)]
pub struct Registry {
    formats: Vec<Box<dyn Format>>,
}

impl Registry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A registry with the built-in formats: `csv`, `json`, `sql` and `markdown`.
    #[must_use]
    pub fn with_builtin() -> Self {
        let mut registry = Self::new();
        let builtin: [Box<dyn Format>; 4] = [
            Box::new(CsvFormat),
            Box::new(JsonFormat),
            Box::new(SqlFormat),
            Box::new(MarkdownFormat),
        ];
        for format in builtin {
            // The ids are distinct, so this cannot fail.
            let _ = registry.register(format);
        }
        registry
    }

    /// Adds a format.
    ///
    /// # Errors
    ///
    /// [`ExportError::DuplicateFormat`] if the id is taken.
    pub fn register(&mut self, format: Box<dyn Format>) -> Result<(), ExportError> {
        if self.get(format.id()).is_some() {
            return Err(ExportError::DuplicateFormat(format.id().to_owned()));
        }
        self.formats.push(format);
        Ok(())
    }

    /// The format with this id.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&dyn Format> {
        self.formats.iter().map(|f| &**f).find(|f| f.id() == id)
    }

    /// Every format, in registration order.
    pub fn formats(&self) -> impl Iterator<Item = &dyn Format> {
        self.formats.iter().map(|f| &**f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_formats_in_order() {
        let r = Registry::with_builtin();
        let ids: Vec<_> = r.formats().map(Format::id).collect();
        assert_eq!(ids, [CSV, JSON, SQL, MARKDOWN]);
        let ext: Vec<_> = r.formats().map(Format::extension).collect();
        assert_eq!(ext, ["csv", "json", "sql", "md"]);
        assert!(r.formats().all(|f| !f.label().is_empty()));
        assert_eq!(r.get("json").unwrap().id(), "json");
        assert!(r.get("xlsx").is_none());
        assert_eq!(Registry::new().formats().count(), 0);
    }

    #[test]
    fn duplicate_ids_are_rejected() {
        let mut r = Registry::with_builtin();
        assert!(matches!(
            r.register(Box::new(CsvFormat)),
            Err(ExportError::DuplicateFormat(id)) if id == "csv"
        ));
        assert_eq!(r.formats().count(), 4);
    }
}
