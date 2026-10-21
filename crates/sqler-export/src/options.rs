// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Options of the built-in formats.

use std::num::NonZeroUsize;

use sqler_sql::Dialect;

/// Options for every format. Each format reads only its own part.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ExportOptions {
    /// `CSV` options.
    pub csv: CsvOptions,
    /// `SQL` `INSERT` options.
    pub sql: SqlOptions,
    /// Markdown table options.
    pub markdown: MarkdownOptions,
    /// `Parquet` options.
    pub parquet: ParquetOptions,
}

/// The line ending of a `CSV` file.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LineEnding {
    /// `\r\n`, as RFC 4180 says.
    #[default]
    Crlf,
    /// `\n`.
    Lf,
}

impl LineEnding {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Crlf => "\r\n",
            Self::Lf => "\n",
        }
    }
}

/// `CSV` options (RFC 4180).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CsvOptions {
    /// The field separator. Must be an ASCII char other than `"`, CR and LF.
    pub delimiter: u8,
    /// Whether the first record holds the column names.
    pub header: bool,
    /// The text of a `NULL`. It must not contain the delimiter, `"`, CR or LF. A non-null
    /// value that renders as this text is quoted, so it stays distinguishable.
    pub null_text: String,
    /// The line ending of every record.
    pub line_ending: LineEnding,
    /// Prefix text cells that start with `=`, `+`, `-`, `@`, tab or CR with a single quote, so a
    /// spreadsheet does not read them as a formula (CSV injection). Off by default. Only text
    /// and `JSON` values and the header are changed; numbers are never touched.
    pub neutralize_formulas: bool,
}

impl Default for CsvOptions {
    fn default() -> Self {
        Self {
            delimiter: b',',
            header: true,
            null_text: String::new(),
            line_ending: LineEnding::Crlf,
            neutralize_formulas: false,
        }
    }
}

/// `SQL` `INSERT` options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlOptions {
    /// The dialect of identifiers and literals.
    pub dialect: Dialect,
    /// The schema (or database on `MySQL`) of the table, if any.
    pub schema: Option<String>,
    /// The table name. Required.
    pub table: String,
    /// How many rows one `INSERT` statement holds.
    pub rows_per_statement: NonZeroUsize,
}

impl Default for SqlOptions {
    fn default() -> Self {
        Self {
            dialect: Dialect::Postgres,
            schema: None,
            table: String::new(),
            rows_per_statement: NonZeroUsize::MIN.saturating_add(99),
        }
    }
}

/// Markdown table options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkdownOptions {
    /// The text of a `NULL`.
    pub null_text: String,
}

impl Default for MarkdownOptions {
    fn default() -> Self {
        Self {
            null_text: "NULL".to_owned(),
        }
    }
}

/// The compression of the `Parquet` column chunks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum ParquetCompression {
    /// No compression.
    Uncompressed,
    /// Snappy: fast, a fair ratio.
    #[default]
    Snappy,
}

/// `Parquet` options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParquetOptions {
    /// How many rows one row group holds at most.
    pub row_group_rows: NonZeroUsize,
    /// The compression of every column chunk.
    pub compression: ParquetCompression,
}

impl Default for ParquetOptions {
    fn default() -> Self {
        Self {
            row_group_rows: NonZeroUsize::MIN.saturating_add(65_535),
            compression: ParquetCompression::default(),
        }
    }
}
