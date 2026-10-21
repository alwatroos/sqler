// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Copies a table of awkward values read through the real `SQLite` driver, in every copy format,
//! and compares the text with what the file-export writers make of the same rows.

// Helpers outside `#[test]` fns also unwrap; clippy.toml only exempts the test fns themselves.
#![allow(clippy::unwrap_used)]

use std::time::{Duration, Instant};

use sqler_driver_api::DriverRegistry;
use sqler_driver_api::sqler_core::{ConnectionConfig, DriverId, Query, Row};
use sqler_driver_sqlite::{DRIVER_ID, SqliteDriver};
use sqler_export::{
    CsvFormat, ExportOptions, Format, JsonFormat, LineEnding, MarkdownFormat, SqlFormat,
    export_to_vec,
};
use sqler_sql::Dialect;
use sqler_ui::grid::{
    ColumnLayout, CopyFormat, CopyLimits, DisplayMap, GridModel, IdentityRows, IngestState,
    InsertTarget, Selection, copy_now, grid_channel, ingest,
};

const SQL: &str = "SELECT 1 AS id, 'plain' AS t, NULL AS n, x'00ff10' AS b \
    UNION ALL SELECT 2, 'tab\tin', 'NULL', x'' \
    UNION ALL SELECT 3, 'line' || char(10) || 'break', '', x'ff' \
    UNION ALL SELECT 4, 'say \"hi\", | pipe', 'it''s', x'00' \
    UNION ALL SELECT 5, 'crlf' || char(13) || char(10) || 'x', '=1+1', NULL \
    UNION ALL SELECT 6, 'emoji \u{1F600} \u{202E} end', '{\"a\": [1, 2]}', x'0102'";

/// The result of `SQL` in a finished grid model.
fn load() -> GridModel {
    let (mut model, sink) = grid_channel();
    let worker = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let mut registry = DriverRegistry::new();
                registry.register(Box::new(SqliteDriver::new())).unwrap();
                let config =
                    ConnectionConfig::new("t", DriverId::new(DRIVER_ID).unwrap(), ":memory:");
                let mut session = registry.connect(&config).await.unwrap();
                let query = Query::new(SQL);
                let mut stream = session.execute(&query).await.unwrap();
                ingest(&mut *stream, sink).await.unwrap();
            });
    });
    let deadline = Instant::now() + Duration::from_secs(20);
    while !matches!(model.state(), IngestState::Done { .. }) {
        assert!(Instant::now() < deadline, "stuck in {:?}", model.state());
        model.poll(usize::MAX);
        std::thread::sleep(Duration::from_millis(1));
    }
    worker.join().unwrap();
    model
}

/// The text the export writers make of `rows` with the options a copy uses.
fn export(model: &GridModel, format: CopyFormat) -> String {
    let rows: Vec<Row> = (0..model.row_count())
        .map(|i| {
            let row = model.row(i).unwrap();
            Row((0..model.columns().len())
                .map(|c| row.get(c).unwrap().to_value())
                .collect())
        })
        .collect();
    let mut options = ExportOptions::default();
    let writer: &dyn Format = match format {
        CopyFormat::Tsv | CopyFormat::TsvWithHeaders => {
            options.csv.delimiter = b'\t';
            options.csv.header = format == CopyFormat::TsvWithHeaders;
            "NULL".clone_into(&mut options.csv.null_text);
            options.csv.line_ending = LineEnding::Lf;
            &CsvFormat
        }
        CopyFormat::Markdown => &MarkdownFormat,
        CopyFormat::Json => &JsonFormat,
        CopyFormat::Insert => {
            options.sql.dialect = Dialect::Sqlite;
            "table_name".clone_into(&mut options.sql.table);
            &SqlFormat
        }
    };
    let mut text =
        String::from_utf8(export_to_vec(writer, &options, model.columns(), &rows).unwrap())
            .unwrap();
    if text.ends_with('\n') && matches!(format, CopyFormat::Tsv | CopyFormat::TsvWithHeaders) {
        text.pop();
    }
    text
}

#[test]
fn every_format_copies_what_the_export_writers_make() {
    let model = load();
    assert_eq!(model.row_count(), 6);
    let columns = ColumnLayout::new(vec![100.0; model.columns().len()]);
    let rows = IdentityRows(model.row_count());
    let map = DisplayMap {
        rows: &rows,
        columns: &columns,
    };
    let mut selection = Selection::default();
    selection.select_all();
    for format in CopyFormat::ALL {
        let target = InsertTarget::placeholder(Some(Dialect::Sqlite));
        let out = copy_now(
            &model,
            &map,
            &selection,
            format,
            target,
            CopyLimits::DEFAULT,
        )
        .unwrap();
        assert_eq!(out.rows, 6, "{format:?}");
        assert_eq!(out.text, export(&model, format), "{format:?}");
    }
}
