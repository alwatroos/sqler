// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Reads the settings file into a [`SourceReport`]. Bounded and tolerant: it never panics.

use std::collections::HashMap;

use serde_json::Value;

use super::{jsonc, map};
use crate::MAX_ENTRIES;
use crate::error::ImportError;
use crate::model::SourceReport;

const NOT_SETTINGS: &str =
    "not an Azure Data Studio settings.json: no \"datasource.connections\" list";

pub(super) fn parse(bytes: &[u8]) -> Result<SourceReport, ImportError> {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    if std::str::from_utf8(bytes).is_err() {
        return Err(ImportError::NotUtf8);
    }
    let json = jsonc::to_json(bytes)?;
    let root: Value = serde_json::from_slice(&json).map_err(|e| malformed(&e))?;
    let Some(root) = root.as_object() else {
        return Err(ImportError::UnrecognisedFormat(NOT_SETTINGS));
    };
    let connections = root.get("datasource.connections");
    let group_list = root.get("datasource.connectionGroups");
    if connections.is_none() && group_list.is_none() {
        return Err(ImportError::UnrecognisedFormat(NOT_SETTINGS));
    }
    let empty = Vec::new();
    let connections = match connections {
        None | Some(Value::Null) => &empty,
        Some(Value::Array(a)) => a,
        Some(_) => return Err(ImportError::UnrecognisedFormat(NOT_SETTINGS)),
    };
    let group_list = match group_list {
        None | Some(Value::Null) => &empty,
        Some(Value::Array(a)) => a,
        Some(_) => return Err(ImportError::UnrecognisedFormat(NOT_SETTINGS)),
    };
    if connections.len() > MAX_ENTRIES || group_list.len() > MAX_ENTRIES {
        return Err(ImportError::TooManyEntries { limit: MAX_ENTRIES });
    }

    let mut notices = Vec::new();
    let mut groups: HashMap<String, map::Group> = HashMap::new();
    let mut ignored = 0_usize;
    for item in group_list {
        match map::group(item) {
            Some((id, group)) => {
                groups.entry(id).or_insert(group);
            }
            None => ignored += 1,
        }
    }
    if ignored > 0 {
        notices.push(format!(
            "{ignored} connection group(s) without a usable id or name were ignored"
        ));
    }

    let entries = connections
        .iter()
        .enumerate()
        .map(|(index, value)| map::map_entry(index, value, &groups))
        .collect();
    Ok(SourceReport {
        source: "azure-data-studio",
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
