// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Hand-walked extraction from a `toml::Table` into typed values.

use std::collections::HashSet;

use sqler_core::ObjectKind;
use toml::{Table, Value};

use super::ManifestError as E;
use super::{Entry, Fields};
use crate::api::ApiVersion;
use crate::capability::{Capability, CapabilitySet};
use crate::contribution::{
    AiTool, Alignment, Command, CompletionContribution, Contributions, DriverContribution,
    DriverKind, Exporter, MenuEntry, MenuLocation, Panel, SettingSpec, SettingValue, StatusItem,
};
use crate::fs_scope::check_relative;
use crate::id::{ContributionId, EngineId, IdError, PluginId, SettingKey, ToolName, truncate};
use crate::limits::{Limits, ManifestLimits};

type R<T> = Result<T, E>;

const LIM: ManifestLimits = Limits::DEFAULT.manifest;

fn join(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_owned()
    } else {
        format!("{path}.{key}")
    }
}

/// Fails with `TooDeep` when tables and arrays nest deeper than `max`. Recursion is bounded by `max`.
pub(super) fn check_depth(v: &Value, depth: usize, max: usize) -> R<()> {
    match v {
        Value::Table(t) => {
            if depth > max {
                return Err(E::TooDeep);
            }
            for x in t.values() {
                check_depth(x, depth + 1, max)?;
            }
        }
        Value::Array(a) => {
            if depth > max {
                return Err(E::TooDeep);
            }
            for x in a {
                check_depth(x, depth + 1, max)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn check_keys(t: &Table, path: &str, allowed: &[&str]) -> R<()> {
    for k in t.keys() {
        if !allowed.contains(&k.as_str()) {
            return Err(E::UnknownField {
                field: join(path, &truncate(k)),
            });
        }
    }
    Ok(())
}

fn bad_text(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}' | '\u{061C}')
}

/// Display text: non-empty, no control or bidi characters, at most `max` characters (bytes if `bytes`).
fn text_value(s: &str, field: &str, max: usize, bytes: bool) -> R<()> {
    if s.is_empty() || s.chars().any(bad_text) {
        return Err(E::BadText {
            field: field.to_owned(),
        });
    }
    let len = if bytes { s.len() } else { s.chars().count() };
    if len > max {
        return Err(E::TooLong {
            field: field.to_owned(),
            max,
        });
    }
    Ok(())
}

fn get<'a>(t: &'a Table, path: &str, key: &str) -> Option<(&'a Value, String)> {
    t.get(key).map(|v| (v, join(path, key)))
}

fn req<'a>(t: &'a Table, path: &str, key: &str) -> R<(&'a Value, String)> {
    get(t, path, key).ok_or_else(|| E::MissingField {
        field: join(path, key),
    })
}

fn wrong(field: String, expected: &'static str) -> E {
    E::WrongType { field, expected }
}

fn as_str<'a>(v: &'a Value, field: &str) -> R<&'a str> {
    v.as_str()
        .ok_or_else(|| wrong(field.to_owned(), "a string"))
}

fn req_str<'a>(t: &'a Table, path: &str, key: &str) -> R<(&'a str, String)> {
    let (v, f) = req(t, path, key)?;
    Ok((as_str(v, &f)?, f))
}

fn opt_str<'a>(t: &'a Table, path: &str, key: &str) -> R<Option<(&'a str, String)>> {
    match get(t, path, key) {
        None => Ok(None),
        Some((v, f)) => Ok(Some((as_str(v, &f)?, f))),
    }
}

fn req_title(t: &Table, path: &str, key: &str, max: usize) -> R<String> {
    let (s, f) = req_str(t, path, key)?;
    text_value(s, &f, max, false)?;
    Ok(s.to_owned())
}

fn opt_title(t: &Table, path: &str, key: &str, max: usize, bytes: bool) -> R<Option<String>> {
    match opt_str(t, path, key)? {
        None => Ok(None),
        Some((s, f)) => {
            text_value(s, &f, max, bytes)?;
            Ok(Some(s.to_owned()))
        }
    }
}

fn id<T: std::str::FromStr<Err = IdError>>(s: &str, field: &str) -> R<T> {
    s.parse().map_err(|reason| E::BadId {
        field: field.to_owned(),
        reason,
    })
}

fn req_id<T: std::str::FromStr<Err = IdError>>(t: &Table, path: &str, key: &str) -> R<T> {
    let (s, f) = req_str(t, path, key)?;
    id(s, &f)
}

/// A list of tables, bounded by the list limit.
fn tables<'a>(root: &'a Table, key: &str) -> R<Vec<(&'a Table, String)>> {
    let Some((v, f)) = get(root, "", key) else {
        return Ok(Vec::new());
    };
    let arr = v
        .as_array()
        .ok_or_else(|| wrong(f.clone(), "an array of tables"))?;
    if arr.len() > LIM.max_list_len {
        return Err(E::TooMany {
            field: f,
            max: LIM.max_list_len,
        });
    }
    arr.iter()
        .enumerate()
        .map(|(i, item)| {
            let p = format!("{key}[{i}]");
            item.as_table()
                .map(|t| (t, p.clone()))
                .ok_or_else(|| wrong(p, "a table"))
        })
        .collect()
}

fn string_list(
    t: &Table,
    path: &str,
    key: &str,
    max_len: usize,
) -> R<Option<Vec<(String, String)>>> {
    let Some((v, f)) = get(t, path, key) else {
        return Ok(None);
    };
    let arr = v
        .as_array()
        .ok_or_else(|| wrong(f.clone(), "an array of strings"))?;
    if arr.len() > max_len {
        return Err(E::TooMany {
            field: f,
            max: max_len,
        });
    }
    let mut out = Vec::with_capacity(arr.len());
    for (i, item) in arr.iter().enumerate() {
        let fi = format!("{f}[{i}]");
        let s = item.as_str().ok_or_else(|| wrong(fi.clone(), "a string"))?;
        out.push((s.to_owned(), fi));
    }
    Ok(Some(out))
}

fn dup<T: std::hash::Hash + Eq + Clone + ToString>(
    set: &mut HashSet<T>,
    kind: &'static str,
    id: &T,
) -> R<()> {
    if set.insert(id.clone()) {
        Ok(())
    } else {
        Err(E::DuplicateContribution {
            kind,
            id: id.to_string(),
        })
    }
}

pub(super) fn walk_plugin(root: &Table) -> R<Fields> {
    let allowed = [
        "plugin",
        "entry",
        "commands",
        "panels",
        "status-items",
        "menus",
        "exporters",
        "completion",
        "ai-tools",
        "drivers",
        "settings",
    ];
    check_keys(root, "", &allowed)?;
    let (pv, _) = req(root, "", "plugin")?;
    let p = pv
        .as_table()
        .ok_or_else(|| wrong("plugin".into(), "a table"))?;
    check_keys(
        p,
        "plugin",
        &[
            "id",
            "name",
            "version",
            "api",
            "description",
            "authors",
            "license",
            "capabilities",
        ],
    )?;
    let id: PluginId = req_id(p, "plugin", "id")?;
    let name = req_title(p, "plugin", "name", LIM.max_name_chars)?;
    let (vs, _) = req_str(p, "plugin", "version")?;
    let version = if vs.len() > 64 {
        None
    } else {
        vs.parse::<semver::Version>().ok()
    }
    .ok_or_else(|| E::BadVersion {
        value: truncate(vs),
    })?;
    let (aps, _) = req_str(p, "plugin", "api")?;
    let api: ApiVersion = aps.parse().map_err(|_| E::BadApi {
        value: truncate(aps),
    })?;
    let description = opt_title(p, "plugin", "description", LIM.max_description_bytes, true)?;
    let mut authors = Vec::new();
    if let Some(list) = string_list(p, "plugin", "authors", LIM.max_list_len)? {
        for (s, f) in list {
            text_value(&s, &f, LIM.max_name_chars, false)?;
            authors.push(s);
        }
    }
    let license = opt_title(p, "plugin", "license", LIM.max_name_chars, false)?;
    let capabilities = walk_capabilities(p)?;
    let entry = match get(root, "", "entry") {
        None => None,
        Some((v, f)) => Some(walk_entry(v, &f)?),
    };
    let contributes = walk_contributions(root)?;
    cross_checks(&capabilities, entry.as_ref(), &contributes)?;
    Ok(Fields {
        id,
        name,
        version,
        api,
        description,
        authors,
        license,
        capabilities,
        entry,
        contributes,
    })
}

fn walk_capabilities(p: &Table) -> R<CapabilitySet> {
    let (v, f) = req(p, "plugin", "capabilities")?;
    let arr = v
        .as_array()
        .ok_or_else(|| wrong(f.clone(), "an array of strings"))?;
    if arr.len() > LIM.max_capabilities {
        return Err(E::TooMany {
            field: f,
            max: LIM.max_capabilities,
        });
    }
    let mut set = CapabilitySet::new();
    for (index, item) in arr.iter().enumerate() {
        let s = item
            .as_str()
            .ok_or_else(|| wrong(format!("{f}[{index}]"), "a string"))?;
        let cap: Capability = s
            .parse()
            .map_err(|source| E::Capability { index, source })?;
        let shown = cap.to_string();
        if !set.insert(cap) {
            return Err(E::DuplicateCapability {
                capability: truncate(&shown),
            });
        }
    }
    if set.allows(crate::capability::CapabilityKind::DbWrite)
        && !set.allows(crate::capability::CapabilityKind::DbRead)
    {
        return Err(E::CapabilityRequires {
            capability: "db.write".into(),
            requires: "db.read".into(),
        });
    }
    Ok(set)
}

fn walk_entry(v: &Value, f: &str) -> R<Entry> {
    let t = v.as_table().ok_or_else(|| wrong(f.to_owned(), "a table"))?;
    check_keys(t, f, &["component"])?;
    let (s, _) = req_str(t, f, "component")?;
    let bad = |reason| E::BadEntryPath {
        path: truncate(s),
        reason,
    };
    check_relative(s).map_err(bad)?;
    if s.strip_suffix(".wasm").is_none_or(str::is_empty) {
        return Err(bad(crate::fs_scope::ScopeError::BadCharacter));
    }
    Ok(Entry {
        component: s.to_owned(),
    })
}

// One flat pass per table keeps the document order of the checks visible.
#[allow(clippy::too_many_lines)]
fn walk_contributions(root: &Table) -> R<Contributions> {
    let mut c = Contributions::default();
    let mut seen = HashSet::new();
    for (t, p) in tables(root, "commands")? {
        check_keys(t, &p, &["id", "title"])?;
        let cid: ContributionId = req_id(t, &p, "id")?;
        dup(&mut seen, "command", &cid)?;
        let title = req_title(t, &p, "title", LIM.max_name_chars)?;
        c.commands.push(Command { id: cid, title });
    }
    let mut seen = HashSet::new();
    for (t, p) in tables(root, "panels")? {
        check_keys(t, &p, &["id", "title"])?;
        let cid: ContributionId = req_id(t, &p, "id")?;
        dup(&mut seen, "panel", &cid)?;
        let title = req_title(t, &p, "title", LIM.max_name_chars)?;
        c.panels.push(Panel { id: cid, title });
    }
    let mut seen = HashSet::new();
    for (t, p) in tables(root, "status-items")? {
        check_keys(t, &p, &["id", "tooltip", "alignment"])?;
        let cid: ContributionId = req_id(t, &p, "id")?;
        dup(&mut seen, "status item", &cid)?;
        let tooltip = opt_title(t, &p, "tooltip", LIM.max_tooltip_chars, false)?;
        let alignment = match opt_str(t, &p, "alignment")? {
            None => Alignment::default(),
            Some(("left", _)) => Alignment::Left,
            Some(("right", _)) => Alignment::Right,
            Some((_, f)) => return Err(wrong(f, "\"left\" or \"right\"")),
        };
        c.status_items.push(StatusItem {
            id: cid,
            tooltip,
            alignment,
        });
    }
    for (t, p) in tables(root, "menus")? {
        check_keys(t, &p, &["command", "location", "object-kinds"])?;
        let (s, _) = req_str(t, &p, "command")?;
        let command: ContributionId = id(s, &join(&p, "command"))?;
        if !c.commands.iter().any(|x| x.id == command) {
            return Err(E::UnknownCommand { menu: p });
        }
        let (loc, lf) = req_str(t, &p, "location")?;
        let location = match loc {
            "editor" => MenuLocation::Editor,
            "results" => MenuLocation::Results,
            "object-browser" => MenuLocation::ObjectBrowser,
            _ => return Err(wrong(lf, "\"editor\", \"results\" or \"object-browser\"")),
        };
        let mut object_kinds = Vec::new();
        if let Some(list) = string_list(t, &p, "object-kinds", LIM.max_list_len)? {
            if location != MenuLocation::ObjectBrowser {
                return Err(E::UnknownField {
                    field: join(&p, "object-kinds"),
                });
            }
            for (s, f) in list {
                object_kinds
                    .push(ObjectKind::from_id(&s).ok_or_else(|| wrong(f, "an object kind"))?);
            }
        }
        c.menus.push(MenuEntry {
            command,
            location,
            object_kinds,
        });
    }
    let mut seen = HashSet::new();
    for (t, p) in tables(root, "exporters")? {
        check_keys(t, &p, &["id", "label", "extension"])?;
        let cid: ContributionId = req_id(t, &p, "id")?;
        dup(&mut seen, "exporter", &cid)?;
        let label = req_title(t, &p, "label", LIM.max_name_chars)?;
        let (ext, ef) = req_str(t, &p, "extension")?;
        if ext.is_empty() {
            return Err(E::BadId {
                field: ef,
                reason: IdError::Empty,
            });
        }
        if ext.len() > 16 {
            return Err(E::BadId {
                field: ef,
                reason: IdError::TooLong,
            });
        }
        if !ext
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        {
            return Err(E::BadId {
                field: ef,
                reason: IdError::BadCharacter,
            });
        }
        c.exporters.push(Exporter {
            id: cid,
            label,
            extension: ext.to_owned(),
        });
    }
    if let Some((v, f)) = get(root, "", "completion") {
        let t = v.as_table().ok_or_else(|| wrong(f.clone(), "a table"))?;
        check_keys(t, &f, &["drivers"])?;
        let mut drivers = Vec::new();
        if let Some(list) = string_list(t, &f, "drivers", LIM.max_list_len)? {
            for (s, fi) in list {
                drivers.push(id::<EngineId>(&s, &fi)?);
            }
        }
        c.completion = Some(CompletionContribution { drivers });
    }
    let mut seen = HashSet::new();
    for (t, p) in tables(root, "ai-tools")? {
        check_keys(t, &p, &["name", "description", "input-schema"])?;
        let name: ToolName = req_id(t, &p, "name")?;
        dup(&mut seen, "AI tool", &name)?;
        let (d, df) = req_str(t, &p, "description")?;
        text_value(d, &df, LIM.max_description_bytes, true)?;
        let description = d.to_owned();
        let (sv, sf) = req(t, &p, "input-schema")?;
        let st = sv.as_table().ok_or_else(|| wrong(sf, "a table"))?;
        let tool = truncate(name.as_str());
        let bad = || E::BadSchema { tool: tool.clone() };
        let json = to_json(&Value::Table(st.clone()), 0).ok_or_else(bad)?;
        let size = serde_json::to_string(&json).map_err(|_| bad())?.len();
        if size > LIM.max_schema_bytes {
            return Err(bad());
        }
        c.ai_tools.push(AiTool {
            name,
            description,
            input_schema: json,
        });
    }
    let mut seen = HashSet::new();
    for (t, p) in tables(root, "drivers")? {
        check_keys(t, &p, &["id", "name", "kind", "engine", "dialect"])?;
        let cid: ContributionId = req_id(t, &p, "id")?;
        dup(&mut seen, "driver", &cid)?;
        let name = req_title(t, &p, "name", LIM.max_name_chars)?;
        let (k, kf) = req_str(t, &p, "kind")?;
        let kind = match k {
            "host" => DriverKind::Host {
                engine: req_id(t, &p, "engine")?,
            },
            "guest" => {
                if t.contains_key("engine") {
                    return Err(E::UnknownField {
                        field: join(&p, "engine"),
                    });
                }
                DriverKind::Guest
            }
            _ => return Err(wrong(kf, "\"host\" or \"guest\"")),
        };
        let dialect = opt_title(t, &p, "dialect", LIM.max_name_chars, false)?;
        c.drivers.push(DriverContribution {
            id: cid,
            name,
            kind,
            dialect,
        });
    }
    let mut seen = HashSet::new();
    for (t, p) in tables(root, "settings")? {
        check_keys(t, &p, &["key", "label", "description", "default"])?;
        let key: SettingKey = req_id(t, &p, "key")?;
        dup(&mut seen, "setting", &key)?;
        let label = req_title(t, &p, "label", LIM.max_name_chars)?;
        let description = opt_title(t, &p, "description", LIM.max_description_bytes, true)?;
        let (dv, df) = req(t, &p, "default")?;
        let default = match dv {
            Value::Boolean(b) => SettingValue::Boolean(*b),
            Value::Integer(i) => SettingValue::Integer(*i),
            Value::Float(x) if x.is_finite() => SettingValue::Float(*x),
            Value::String(s) => {
                if s.chars().any(bad_text) {
                    return Err(E::BadText { field: df });
                }
                if s.len() > LIM.max_description_bytes {
                    return Err(E::TooLong {
                        field: df,
                        max: LIM.max_description_bytes,
                    });
                }
                SettingValue::Text(s.clone())
            }
            _ => return Err(wrong(df, "a boolean, integer, finite float or string")),
        };
        c.settings.push(SettingSpec {
            key,
            label,
            description,
            default,
        });
    }
    Ok(c)
}

/// TOML to JSON; `None` for non-finite floats or nesting beyond the limit.
fn to_json(v: &Value, depth: usize) -> Option<serde_json::Value> {
    if depth > LIM.max_depth {
        return None;
    }
    Some(match v {
        Value::String(s) => serde_json::Value::String(s.clone()),
        Value::Integer(i) => serde_json::Value::from(*i),
        Value::Float(x) => serde_json::Value::Number(serde_json::Number::from_f64(*x)?),
        Value::Boolean(b) => serde_json::Value::Bool(*b),
        Value::Datetime(d) => serde_json::Value::String(d.to_string()),
        Value::Array(a) => serde_json::Value::Array(
            a.iter()
                .map(|x| to_json(x, depth + 1))
                .collect::<Option<Vec<_>>>()?,
        ),
        Value::Table(t) => serde_json::Value::Object(
            t.iter()
                .map(|(k, x)| to_json(x, depth + 1).map(|j| (k.clone(), j)))
                .collect::<Option<serde_json::Map<_, _>>>()?,
        ),
    })
}

fn cross_checks(caps: &CapabilitySet, entry: Option<&Entry>, c: &Contributions) -> R<()> {
    use crate::capability::CapabilityKind as K;
    let need = |what: &str, kind: K, present: bool| -> R<()> {
        if present && !caps.allows(kind) {
            return Err(E::MissingCapability {
                contribution: what.to_owned(),
                capability: kind.name().to_owned(),
            });
        }
        Ok(())
    };
    need("panels", K::UiPanel, !c.panels.is_empty())?;
    need("ai-tools", K::AiTool, !c.ai_tools.is_empty())?;
    for (i, d) in c.drivers.iter().enumerate() {
        match &d.kind {
            DriverKind::Host { engine } => {
                if !caps.contains(&Capability::DriverHost(engine.clone())) {
                    return Err(E::MissingCapability {
                        contribution: format!("drivers[{i}]"),
                        capability: format!("driver.host:{engine}"),
                    });
                }
            }
            DriverKind::Guest => need(&format!("drivers[{i}]"), K::DriverGuest, true)?,
        }
    }
    if entry.is_none() {
        let code = [
            ("commands", !c.commands.is_empty()),
            ("panels", !c.panels.is_empty()),
            ("status-items", !c.status_items.is_empty()),
            ("menus", !c.menus.is_empty()),
            ("exporters", !c.exporters.is_empty()),
            ("completion", c.completion.is_some()),
            ("ai-tools", !c.ai_tools.is_empty()),
            (
                "drivers",
                c.drivers
                    .iter()
                    .any(|d| matches!(d.kind, DriverKind::Guest)),
            ),
        ];
        for (name, present) in code {
            if present {
                return Err(E::MissingEntry {
                    contribution: name.to_owned(),
                });
            }
        }
    }
    Ok(())
}
