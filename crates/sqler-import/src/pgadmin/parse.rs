// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Reads `servers.json` into a [`SourceReport`]. Bounded and tolerant: it never panics.

use serde_json::Value;

use super::map;
use crate::MAX_ENTRIES;
use crate::error::ImportError;
use crate::model::SourceReport;

const NOT_SERVERS: &str = "not a pgAdmin servers.json: no \"Servers\" object";

pub(super) fn parse(bytes: &[u8]) -> Result<SourceReport, ImportError> {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    if std::str::from_utf8(bytes).is_err() {
        return Err(ImportError::NotUtf8);
    }
    let root: Value = serde_json::from_slice(bytes).map_err(|e| malformed(&e))?;
    let Some(root) = root.as_object() else {
        return Err(ImportError::UnrecognisedFormat(NOT_SERVERS));
    };
    let Some(servers) = root.get("Servers").and_then(Value::as_object) else {
        return Err(ImportError::UnrecognisedFormat(NOT_SERVERS));
    };
    if servers.len() > MAX_ENTRIES {
        return Err(ImportError::TooManyEntries { limit: MAX_ENTRIES });
    }
    let notices = root
        .keys()
        .filter(|k| *k != "Servers")
        .map(|k| format!("unknown top-level key {}", map::safe_name(k)))
        .collect();

    let mut ordered: Vec<(&String, &Value)> = servers.iter().collect();
    ordered.sort_by(
        |(a, _), (b, _)| match (a.parse::<u64>(), b.parse::<u64>()) {
            (Ok(x), Ok(y)) => x.cmp(&y).then_with(|| a.cmp(b)),
            (Ok(_), Err(_)) => std::cmp::Ordering::Less,
            (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
            (Err(_), Err(_)) => a.cmp(b),
        },
    );
    let entries = ordered
        .into_iter()
        .enumerate()
        .map(|(index, (_, value))| map::map_entry(index, value))
        .collect();
    Ok(SourceReport {
        source: "pgadmin",
        entries,
        notices,
    })
}

/// A position and a fixed text per error class; the serde text is not used because it can quote input.
fn malformed(e: &serde_json::Error) -> ImportError {
    use serde_json::error::Category;
    let message = match e.classify() {
        Category::Io => "could not read the data",
        Category::Syntax => "invalid JSON syntax or nesting too deep",
        Category::Data => "unexpected data",
        Category::Eof => "unexpected end of file",
    };
    ImportError::Malformed {
        line: e.line(),
        column: e.column(),
        message: message.to_owned(),
    }
}
