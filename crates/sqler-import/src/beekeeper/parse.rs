// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Reads the connection export into a [`SourceReport`]. Bounded and tolerant: it never panics.

use std::collections::HashMap;

use serde_json::Value;

use super::map;
use crate::MAX_ENTRIES;
use crate::error::ImportError;
use crate::model::SourceReport;

const NOT_EXPORT: &str = "not a Beekeeper Studio export: no list of connections";

/// Keys that hold the connection list, then the folder list.
const CONNECTION_KEYS: [&str; 2] = ["connections", "savedConnections"];
const FOLDER_KEYS: [&str; 3] = ["connection_folders", "connectionFolders", "folders"];

/// A folder: its name and the id of its parent.
pub(super) struct Folder {
    pub(super) name: String,
    pub(super) parent: Option<String>,
}

/// The folders of the export by id.
#[derive(Default)]
pub(super) struct Folders(pub(super) HashMap<String, Folder>);

/// An id as text: numbers and strings are accepted.
pub(super) fn id_text(v: &Value) -> Option<String> {
    match v {
        Value::Number(n) => Some(n.to_string()),
        Value::String(s) if !s.is_empty() && s.chars().count() <= 64 => Some(s.clone()),
        _ => None,
    }
}

fn folders_from(list: &[Value]) -> Folders {
    let mut map = HashMap::new();
    for item in list {
        let Some(obj) = item.as_object() else {
            continue;
        };
        let Some(id) = obj.get("id").and_then(id_text) else {
            continue;
        };
        let name = obj
            .get("name")
            .and_then(Value::as_str)
            .filter(|n| n.chars().count() <= crate::MAX_FIELD_CHARS)
            .map(map::clean_name)
            .unwrap_or_default();
        let parent = obj
            .get("parentId")
            .or_else(|| obj.get("parent_id"))
            .and_then(id_text);
        map.insert(id, Folder { name, parent });
    }
    Folders(map)
}

pub(super) fn parse(bytes: &[u8]) -> Result<SourceReport, ImportError> {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    if std::str::from_utf8(bytes).is_err() {
        return Err(ImportError::NotUtf8);
    }
    let root: Value = serde_json::from_slice(bytes).map_err(|e| malformed(&e))?;
    let mut notices = Vec::new();
    let mut folders = Folders::default();
    let list: &[Value] = match &root {
        Value::Array(a) => a,
        Value::Object(o) => {
            let Some(list) = CONNECTION_KEYS
                .iter()
                .find_map(|k| o.get(*k))
                .and_then(Value::as_array)
            else {
                return Err(ImportError::UnrecognisedFormat(NOT_EXPORT));
            };
            if let Some(f) = FOLDER_KEYS
                .iter()
                .find_map(|k| o.get(*k))
                .and_then(Value::as_array)
            {
                if f.len() > MAX_ENTRIES {
                    return Err(ImportError::TooManyEntries { limit: MAX_ENTRIES });
                }
                folders = folders_from(f);
            }
            notices = o
                .keys()
                .filter(|k| {
                    !CONNECTION_KEYS.contains(&k.as_str()) && !FOLDER_KEYS.contains(&k.as_str())
                })
                .map(|k| format!("unknown top-level key {}", map::safe_name(k)))
                .collect();
            list
        }
        _ => return Err(ImportError::UnrecognisedFormat(NOT_EXPORT)),
    };
    if list.len() > MAX_ENTRIES {
        return Err(ImportError::TooManyEntries { limit: MAX_ENTRIES });
    }
    let entries = list
        .iter()
        .enumerate()
        .map(|(index, value)| map::map_entry(index, value, &folders))
        .collect();
    Ok(SourceReport {
        source: "beekeeper",
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
