// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The format registry.

use std::sync::Arc;

use crate::{CsvFormat, ExportError, Format, JsonFormat, MarkdownFormat, SqlFormat};

/// The id of the `CSV` format.
pub const CSV: &str = "csv";
/// The id of the `JSON` format.
pub const JSON: &str = "json";
/// The id of the `SQL` `INSERT` format.
pub const SQL: &str = "sql";
/// The id of the Markdown table format.
pub const MARKDOWN: &str = "markdown";

/// The id of the `Parquet` format.
pub const PARQUET: &str = "parquet";
/// The id of the Excel (`XLSX`) format.
pub const XLSX: &str = "xlsx";

/// Formats that come and go at run time (plugin exporters).
///
/// Called on the UI thread: no I/O, no blocking.
pub trait FormatSource: Send + Sync {
    /// The formats offered right now.
    fn formats(&self) -> Vec<Box<dyn Format>>;
}

/// A list of formats by id, in registration order.
#[derive(Default)]
pub struct Registry {
    formats: Vec<Box<dyn Format>>,
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list()
            .entries(self.formats.iter().map(|format| format.id()))
            .finish()
    }
}

impl Registry {
    /// The built-in formats, then each source's formats in order.
    ///
    /// A format whose id is already taken is skipped silently.
    #[must_use]
    pub fn with_sources(sources: &[Arc<dyn FormatSource>]) -> Self {
        let mut registry = Self::with_builtin();
        for source in sources {
            for format in source.formats() {
                // A taken id is skipped on purpose: the first one wins.
                let _ = registry.register(format);
            }
        }
        registry
    }

    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A registry with the built-in formats: `csv`, `json`, `sql` and `markdown`, then
    /// `parquet` and `xlsx` when their cargo features are on.
    #[must_use]
    pub fn with_builtin() -> Self {
        let mut registry = Self::new();
        let builtin: Vec<Box<dyn Format>> = vec![
            Box::new(CsvFormat),
            Box::new(JsonFormat),
            Box::new(SqlFormat),
            Box::new(MarkdownFormat),
            #[cfg(feature = "parquet")]
            Box::new(crate::ParquetFormat),
            #[cfg(feature = "xlsx")]
            Box::new(crate::XlsxFormat),
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
        let mut want = vec![CSV, JSON, SQL, MARKDOWN];
        let mut want_ext = vec!["csv", "json", "sql", "md"];
        if cfg!(feature = "parquet") {
            want.push(PARQUET);
            want_ext.push("parquet");
        }
        if cfg!(feature = "xlsx") {
            want.push(XLSX);
            want_ext.push("xlsx");
        }
        assert_eq!(ids, want);
        let ext: Vec<_> = r.formats().map(Format::extension).collect();
        assert_eq!(ext, want_ext);
        assert!(r.formats().all(|f| !f.label().is_empty()));
        assert_eq!(r.get("json").unwrap().id(), "json");
        assert_eq!(r.get(XLSX).is_some(), cfg!(feature = "xlsx"));
        assert_eq!(r.get(PARQUET).is_some(), cfg!(feature = "parquet"));
        assert!(r.get("nope").is_none());
        assert_eq!(Registry::new().formats().count(), 0);
    }

    struct Fake(&'static str);

    impl Format for Fake {
        fn id(&self) -> &'static str {
            self.0
        }
        fn label(&self) -> &'static str {
            "Fake"
        }
        fn extension(&self) -> &'static str {
            "fk"
        }
        fn create<'a>(
            &self,
            _options: &crate::ExportOptions,
            _sink: crate::Sink<'a>,
        ) -> Result<Box<dyn crate::RowWriter + 'a>, ExportError> {
            Err(ExportError::InvalidOptions("fake".into()))
        }
    }

    struct Source(Vec<&'static str>);

    impl FormatSource for Source {
        fn formats(&self) -> Vec<Box<dyn Format>> {
            self.0
                .iter()
                .map(|id| Box::new(Fake(id)) as Box<dyn Format>)
                .collect()
        }
    }

    #[test]
    fn sources_come_after_builtins_in_order() {
        let a: Arc<dyn FormatSource> = Arc::new(Source(vec!["plugin-a:x", "plugin-a:y"]));
        let b: Arc<dyn FormatSource> = Arc::new(Source(vec!["plugin-b:z"]));
        let r = Registry::with_sources(&[a, b]);
        let ids: Vec<_> = r.formats().map(Format::id).collect();
        let builtin = Registry::with_builtin().formats().count();
        assert_eq!(
            &ids[..builtin],
            &Registry::with_builtin()
                .formats()
                .map(Format::id)
                .collect::<Vec<_>>()[..]
        );
        assert_eq!(&ids[builtin..], ["plugin-a:x", "plugin-a:y", "plugin-b:z"]);
        assert_eq!(Registry::with_sources(&[]).formats().count(), builtin);
    }

    #[test]
    fn source_duplicate_id_is_skipped() {
        let a: Arc<dyn FormatSource> = Arc::new(Source(vec!["csv", "plugin-a:x", "plugin-a:x"]));
        let r = Registry::with_sources(&[a]);
        let builtin = Registry::with_builtin().formats().count();
        assert_eq!(r.formats().count(), builtin + 1);
        assert_eq!(r.get("csv").unwrap().label(), CsvFormat.label());
        assert_eq!(format!("{:?}", Registry::new()), "[]");
        assert!(format!("{r:?}").contains("plugin-a:x"));
    }

    #[test]
    fn duplicate_ids_are_rejected() {
        let mut r = Registry::with_builtin();
        assert!(matches!(
            r.register(Box::new(CsvFormat)),
            Err(ExportError::DuplicateFormat(id)) if id == "csv"
        ));
        let n = r.formats().count();
        assert!(matches!(
            r.register(Box::new(CsvFormat)),
            Err(ExportError::DuplicateFormat(_))
        ));
        assert_eq!(r.formats().count(), n);
    }
}
