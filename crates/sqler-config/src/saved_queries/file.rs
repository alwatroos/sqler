// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `queries.toml` wire format (version 1), with version dispatch and
//! serialisation.

use std::collections::HashSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::model::{
    SAVED_QUERIES_FORMAT_VERSION, SavedQueries, SavedQuery, SavedQueryId, id_problem,
    normalize_folder, normalize_name, normalize_tags,
};
use super::parse_message;
use crate::{ConfigError, ConnectionId};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    version: u32,
    #[serde(default, rename = "query", skip_serializing_if = "Vec::is_empty")]
    queries: Vec<Wire>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    id: String,
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    folder: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    connection: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tags: Vec<String>,
    sql: String,
}

fn invalid(path: &Path, message: String) -> ConfigError {
    ConfigError::Invalid {
        path: path.to_owned(),
        message,
    }
}

/// Parses the text of `queries.toml`.
pub(super) fn parse(path: &Path, text: &str) -> Result<SavedQueries, ConfigError> {
    let parse_err = |e: &toml::de::Error| ConfigError::Parse {
        path: path.to_owned(),
        message: parse_message(text, e),
    };
    let table: toml::Table = toml::from_str(text).map_err(|e| parse_err(&e))?;
    match table.get("version") {
        None => Err(invalid(path, "missing required key `version`".to_owned())),
        Some(toml::Value::Integer(n)) if *n >= 0 => {
            if *n == i64::from(SAVED_QUERIES_FORMAT_VERSION) {
                let file: File = toml::from_str(text).map_err(|e| parse_err(&e))?;
                convert(path, file)
            } else if *n == 0 {
                Err(invalid(
                    path,
                    "unsupported version 0 (the first format is 1)".to_owned(),
                ))
            } else {
                Err(invalid(
                    path,
                    format!(
                        "written by a newer SQLer (version {n}, this build reads up to {SAVED_QUERIES_FORMAT_VERSION})"
                    ),
                ))
            }
        }
        Some(_) => Err(ConfigError::Parse {
            path: path.to_owned(),
            message: "version must be a non-negative integer".to_owned(),
        }),
    }
}

fn convert(path: &Path, file: File) -> Result<SavedQueries, ConfigError> {
    let mut out = SavedQueries::new();
    let mut ids: HashSet<String> = HashSet::new();
    for (i, w) in file.queries.into_iter().enumerate() {
        let bad = |msg: String| invalid(path, format!("query[{i}] \"{}\": {msg}", w.name));
        if let Some(problem) = id_problem(&w.id) {
            return Err(bad(format!("invalid id: {problem}")));
        }
        if !ids.insert(w.id.clone()) {
            return Err(bad(format!("duplicate id {:?}", w.id)));
        }
        let name = normalize_name(&w.name).map_err(|e| bad(e.to_string()))?;
        let folder = normalize_folder(w.folder.as_deref()).map_err(|e| bad(e.to_string()))?;
        let tags = normalize_tags(&w.tags).map_err(|e| bad(e.to_string()))?;
        let connection = match &w.connection {
            None => None,
            Some(c) if crate::history::valid_connection_id(c) => {
                Some(ConnectionId::from_valid(c.clone()))
            }
            Some(c) => return Err(bad(format!("invalid connection id {c:?}"))),
        };
        out.check_unique(&name, folder.as_deref(), None)
            .map_err(|e| bad(e.to_string()))?;
        out.push_loaded(SavedQuery::from_parts(
            SavedQueryId::from_valid(w.id),
            name,
            folder,
            connection,
            tags,
            w.sql,
        ));
    }
    Ok(out)
}

/// Serialises the queries as a `version = 1` file.
pub(super) fn serialize(path: &Path, queries: &SavedQueries) -> Result<String, ConfigError> {
    let file = File {
        version: SAVED_QUERIES_FORMAT_VERSION,
        queries: queries
            .queries()
            .iter()
            .map(|q| Wire {
                id: q.id().to_string(),
                name: q.name().to_owned(),
                folder: q.folder().map(str::to_owned),
                connection: q.connection().map(ToString::to_string),
                tags: q.tags().to_vec(),
                sql: q.sql().to_owned(),
            })
            .collect(),
    };
    toml::to_string_pretty(&file).map_err(|e| ConfigError::Serialize {
        path: path.to_owned(),
        message: e.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::saved_queries::SavedQueryDraft;

    fn p() -> &'static Path {
        Path::new("queries.toml")
    }

    fn err(text: &str) -> String {
        parse(p(), text).unwrap_err().to_string()
    }

    #[test]
    fn round_trips_every_field_including_multiline_sql() {
        let mut s = SavedQueries::new();
        let mut d = SavedQueryDraft::new("Monthly", "select *\nfrom \"t\"\nwhere a = 'x\\y'");
        d.folder = Some("reports/2026".into());
        d.connection = Some(ConnectionId::from_valid("billing".into()));
        d.tags = vec!["a".into(), "b c".into()];
        s.create(d).unwrap();
        s.create(SavedQueryDraft::new("Plain", "")).unwrap();
        let text = serialize(p(), &s).unwrap();
        assert!(text.starts_with("version = 1\n"), "{text}");
        assert_eq!(parse(p(), &text).unwrap(), s);
    }

    #[test]
    fn empty_file_with_version_is_empty() {
        assert_eq!(parse(p(), "version = 1\n").unwrap().queries(), []);
    }

    #[test]
    fn version_is_required_and_checked() {
        assert!(err("").contains("missing required key `version`"));
        assert!(err("version = 0").contains("unsupported version 0"));
        assert!(err("version = 2").contains("newer SQLer (version 2, this build reads up to 1)"));
        assert!(err("version = \"1\"").contains("non-negative integer"));
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(err("version = 1\nextra = 1").contains("unknown field"));
        assert!(
            err("version = 1\n[[query]]\nid=\"q-1\"\nname=\"a\"\nsql=\"x\"\nbogus=1")
                .contains("unknown field")
        );
    }

    #[test]
    fn entry_errors_name_the_entry() {
        let q = |body: &str| format!("version = 1\n[[query]]\n{body}\n");
        let ok = "id=\"q-1\"\nname=\"a\"\nsql=\"x\"";
        assert!(parse(p(), &q(ok)).is_ok());
        assert!(
            err(&q("id=\"Q 1\"\nname=\"a\"\nsql=\"x\"")).contains("query[0] \"a\": invalid id")
        );
        assert!(err(&q("id=\"q-1\"\nname=\" \"\nsql=\"x\"")).contains("name must not be empty"));
        assert!(
            err(&q("id=\"q-1\"\nname=\"a\"\nsql=\"x\"\nfolder=\"a//b\""))
                .contains("invalid folder")
        );
        assert!(
            err(&q(
                "id=\"q-1\"\nname=\"a\"\nsql=\"x\"\nconnection=\"Bad Id\""
            ))
            .contains("invalid connection id")
        );
        assert!(
            err(&q("id=\"q-1\"\nname=\"a\"\nsql=\"x\"\ntags=[\"\"]"))
                .contains("tags must not be empty")
        );
        let dup_id = format!("{}\n[[query]]\nid=\"q-1\"\nname=\"b\"\nsql=\"y\"", q(ok));
        assert!(err(&dup_id).contains("query[1] \"b\": duplicate id"));
        let dup_name = format!("{}\n[[query]]\nid=\"q-2\"\nname=\"a\"\nsql=\"y\"", q(ok));
        assert!(err(&dup_name).contains("query[1] \"a\": a saved query named"));
    }

    #[test]
    fn hand_edited_values_are_normalised_and_dangling_connection_kept() {
        let text = "version = 1\n[[query]]\nid=\"q-9\"\nname=\" n \"\nsql=\"x\"\nfolder=\" a / b \"\ntags=[\" t\",\"t\"]\nconnection=\"gone\"\n";
        let s = parse(p(), text).unwrap();
        let q = &s.queries()[0];
        assert_eq!(q.name(), "n");
        assert_eq!(q.folder(), Some("a/b"));
        assert_eq!(q.tags(), ["t"]);
        assert_eq!(q.connection().map(ConnectionId::as_str), Some("gone"));
    }

    #[test]
    fn parse_errors_do_not_echo_the_source() {
        let msg = err("version = 1\nsql = \"topsecret\" oops");
        assert!(!msg.contains("topsecret"), "{msg}");
        assert!(msg.contains("line 2"), "{msg}");
    }
}
