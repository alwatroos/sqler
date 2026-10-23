// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Maps one Beekeeper connection to a [`ConnectionSpec`].
//!
//! Secret fields (`password`, `sshPassword`, `sshKeyfilePassword`, URL fields) are never read: the
//! code has no access to them by key, and their names are not even listed.

use std::fmt::Write as _;
use std::net::Ipv6Addr;
use std::path::PathBuf;

use serde_json::{Map, Value};
use sqler_config::{ConnectionSpec, ProfileSecretKind, SshAuth, SshProfile, TlsMode, TlsProfile};
use sqler_core::DriverId;

use super::parse::{Folders, id_text};
use crate::model::{EntryOutcome, SkipReason, SourceEntry};
use crate::path::foreign_path;
use crate::{MAX_FIELD_CHARS, MAX_GROUP_DEPTH};

const DEFAULT_SSH_PORT: u16 = 22;
const MAX_NAME_SHOWN: usize = 64;

/// Keys that are mapped, or are bookkeeping the tool always writes.
const MAPPED: [&str; 26] = [
    "id",
    "createdAt",
    "updatedAt",
    "workspaceId",
    "name",
    "connectionType",
    "host",
    "port",
    "defaultDatabase",
    "databaseFile",
    "username",
    "labelColor",
    "connectionFolderId",
    "connectionFolder",
    "ssl",
    "sslCaFile",
    "sslCertFile",
    "sslKeyFile",
    "sslRejectUnauthorized",
    "sshEnabled",
    "sshHost",
    "sshPort",
    "sshUsername",
    "sshKeyfile",
    "sshMode",
    "sshBastionHost",
];

/// Engines that are known and not supported, with the name shown.
const UNSUPPORTED_ENGINES: [(&str, &str); 17] = [
    ("cockroachdb", "CockroachDB"),
    ("redshift", "Redshift"),
    ("oracle", "Oracle"),
    ("mongodb", "MongoDB"),
    ("cassandra", "Cassandra"),
    ("bigquery", "BigQuery"),
    ("firebird", "Firebird"),
    ("clickhouse", "ClickHouse"),
    ("libsql", "LibSQL"),
    ("sqlanywhere", "SQL Anywhere"),
    ("tidb", "TiDB"),
    ("surrealdb", "SurrealDB"),
    ("trino", "Trino"),
    ("redis", "Redis"),
    ("snowflake", "Snowflake"),
    ("athena", "Athena"),
    ("db2", "Db2"),
];

/// A foreign key name made safe to show: control characters replaced, cut to 64 characters, quoted.
pub(super) fn safe_name(key: &str) -> String {
    let cleaned: String = key
        .chars()
        .take(MAX_NAME_SHOWN)
        .map(|c| if c.is_control() { '?' } else { c })
        .collect();
    let cut = if key.chars().count() > MAX_NAME_SHOWN {
        "..."
    } else {
        ""
    };
    format!("\"{cleaned}\"{cut}")
}

pub(super) fn clean_name(raw: &str) -> String {
    raw.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_owned()
}

/// Percent-encodes everything but the RFC 3986 unreserved characters.
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// Keys whose name says they hold a secret; they are skipped without a mention.
pub(super) fn is_secret_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    ["password", "passphrase", "secret", "token", "url", "uri"]
        .iter()
        .any(|s| k.contains(s))
}

struct Ctx<'a> {
    obj: &'a Map<String, Value>,
    notes: Vec<String>,
}

impl Ctx<'_> {
    /// A string field: `None` when absent, null, of the wrong type or too long (with a note).
    fn text(&mut self, key: &str) -> Option<String> {
        match self.obj.get(key)? {
            Value::Null => None,
            Value::String(s) if s.chars().count() > MAX_FIELD_CHARS => {
                self.notes.push(format!(
                    "field {} is too long and was ignored",
                    safe_name(key)
                ));
                None
            }
            Value::String(s) => Some(s.clone()),
            _ => {
                self.notes.push(format!(
                    "field {} has an unexpected type and was ignored",
                    safe_name(key)
                ));
                None
            }
        }
    }

    /// A non-negative whole number given as a JSON number or a digit string.
    fn number(&self, key: &str) -> Result<Option<u64>, ()> {
        match self.obj.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Number(n)) => n.as_u64().map(Some).ok_or(()),
            Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
            Some(Value::String(s)) => {
                let t = s.trim();
                if t.len() <= 10 && t.bytes().all(|b| b.is_ascii_digit()) {
                    t.parse().map(Some).map_err(|_| ())
                } else {
                    Err(())
                }
            }
            Some(_) => Err(()),
        }
    }

    /// `None` when the key is absent or null.
    fn flag(&mut self, key: &str) -> Option<bool> {
        match self.obj.get(key) {
            None | Some(Value::Null) => None,
            Some(Value::Bool(b)) => Some(*b),
            Some(Value::Number(n)) => Some(n.as_u64() == Some(1)),
            Some(Value::String(s)) => {
                let t = s.trim().to_ascii_lowercase();
                Some(t == "1" || t == "true")
            }
            Some(_) => {
                self.notes.push(format!(
                    "field {} has an unexpected type and was ignored",
                    safe_name(key)
                ));
                None
            }
        }
    }

    fn port(&self, key: &str, default: u16) -> Result<u16, ()> {
        match self.number(key)? {
            None => Ok(default),
            Some(n) => u16::try_from(n).ok().filter(|p| *p != 0).ok_or(()),
        }
    }
}

fn invalid(text: &str) -> SkipReason {
    SkipReason::Invalid(text.to_owned())
}

/// The folder names from the root to the connection's folder.
fn group_of(ctx: &mut Ctx<'_>, folders: &Folders) -> Vec<String> {
    let mut path: Vec<String> = Vec::new();
    if let Some(id) = ctx.obj.get("connectionFolderId").and_then(id_text) {
        let mut seen: Vec<String> = Vec::new();
        let mut current = id;
        loop {
            if seen.contains(&current) || seen.len() >= MAX_GROUP_DEPTH * 2 {
                ctx.notes
                    .push("the folder chain is cyclic or too deep and was cut".to_owned());
                break;
            }
            let Some(folder) = folders.0.get(&current) else {
                break;
            };
            if !folder.name.is_empty() {
                path.push(folder.name.clone());
            }
            let Some(parent) = &folder.parent else {
                break;
            };
            seen.push(current.clone());
            current.clone_from(parent);
        }
        path.reverse();
        return path;
    }
    let named = match ctx.obj.get("connectionFolder") {
        Some(Value::String(s)) => Some(s.as_str()),
        Some(Value::Object(o)) => o.get("name").and_then(Value::as_str),
        _ => None,
    };
    if let Some(n) = named.filter(|n| n.chars().count() <= MAX_FIELD_CHARS) {
        let n = clean_name(n);
        if !n.is_empty() {
            path.push(n);
        }
    }
    path
}

pub(super) fn map_entry(index: usize, value: &Value, folders: &Folders) -> SourceEntry {
    let fallback = format!("Beekeeper connection {}", index + 1);
    let Some(obj) = value.as_object() else {
        return SourceEntry {
            index,
            name: fallback,
            group: Vec::new(),
            outcome: EntryOutcome::Skipped(invalid("the entry is not an object")),
            unsupported: Vec::new(),
            notes: Vec::new(),
            secrets: Vec::new(),
        };
    };
    let mut ctx = Ctx {
        obj,
        notes: Vec::new(),
    };
    let name = ctx
        .text("name")
        .map(|n| clean_name(&n))
        .filter(|n| !n.is_empty())
        .unwrap_or(fallback);
    let group = group_of(&mut ctx, folders);
    let unsupported: Vec<String> = obj
        .keys()
        .filter(|k| !MAPPED.contains(&k.as_str()) && !is_secret_key(k))
        .map(|k| safe_name(k))
        .collect();

    let (outcome, secrets) = match build(&mut ctx, &name) {
        Ok((spec, secrets)) => (EntryOutcome::Mapped(spec), secrets),
        Err(reason) => (EntryOutcome::Skipped(reason), Vec::new()),
    };
    SourceEntry {
        index,
        name,
        group,
        outcome,
        unsupported,
        notes: ctx.notes,
        secrets,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Engine {
    Postgres,
    Mysql,
    Sqlite,
    SqlServer,
}

impl Engine {
    fn driver(self) -> &'static str {
        match self {
            Self::Postgres => "postgres",
            Self::Mysql => "mysql",
            Self::Sqlite => "sqlite",
            Self::SqlServer => "sqlserver",
        }
    }

    fn default_port(self) -> u16 {
        match self {
            Self::Postgres => 5432,
            Self::Mysql => 3306,
            Self::SqlServer => 1433,
            Self::Sqlite => 0,
        }
    }
}

fn engine_of(ctx: &mut Ctx<'_>) -> Result<Engine, SkipReason> {
    let raw = ctx.text("connectionType").unwrap_or_default();
    let t = raw.trim().to_ascii_lowercase();
    match t.as_str() {
        "" => Err(invalid("the connection type is missing")),
        "postgresql" | "postgres" => Ok(Engine::Postgres),
        "mysql" | "mariadb" => Ok(Engine::Mysql),
        "sqlite" => Ok(Engine::Sqlite),
        "sqlserver" | "mssql" => Ok(Engine::SqlServer),
        "duckdb" => Err(SkipReason::NotInThisRelease { engine: "DuckDB" }),
        other => {
            let known = UNSUPPORTED_ENGINES.iter().find(|(id, _)| *id == other);
            Err(SkipReason::Unsupported(match known {
                Some((_, label)) => format!("{label} connections are not supported"),
                None => "this connection type is not supported".to_owned(),
            }))
        }
    }
}

fn build(
    ctx: &mut Ctx<'_>,
    name: &str,
) -> Result<(ConnectionSpec, Vec<ProfileSecretKind>), SkipReason> {
    let engine = engine_of(ctx)?;
    let driver = DriverId::new(engine.driver()).map_err(|_| invalid("driver id"))?;
    if engine == Engine::Sqlite {
        return build_sqlite(ctx, name, driver);
    }
    if ctx.flag("socketPathEnabled") == Some(true) {
        return Err(SkipReason::Unsupported(
            "socket connections are not supported".to_owned(),
        ));
    }
    let host = host_text(&ctx.text("host").unwrap_or_default())?;
    let port = ctx
        .port("port", engine.default_port())
        .map_err(|()| invalid("the port is not valid"))?;
    let db = ctx
        .text("defaultDatabase")
        .map(|d| d.trim().to_owned())
        .filter(|d| !d.is_empty());
    let user = ctx
        .text("username")
        .map(|u| u.trim().to_owned())
        .unwrap_or_default();

    let target = match engine {
        Engine::Postgres => {
            let db = db.unwrap_or_else(|| "postgres".to_owned());
            let mut t = format!("postgres://{host}:{port}/{}", encode(&db));
            if !user.is_empty() {
                let _ = write!(t, "?user={}", encode(&user));
            }
            t
        }
        Engine::Mysql | Engine::SqlServer => {
            let scheme = if engine == Engine::Mysql {
                "mysql"
            } else {
                "sqlserver"
            };
            let at = if user.is_empty() {
                String::new()
            } else {
                format!("{}@", encode(&user))
            };
            let mut t = format!("{scheme}://{at}{host}:{port}");
            if let Some(db) = db {
                let _ = write!(t, "/{}", encode(&db));
            }
            t
        }
        Engine::Sqlite => return Err(invalid("driver id")),
    };
    let mut spec = ConnectionSpec::new(name, driver, target);
    spec.tls = tls(ctx, engine);
    colour(ctx, &mut spec);

    let mut secrets = Vec::new();
    if !user.is_empty() {
        secrets.push(ProfileSecretKind::DbPassword);
    }
    if ctx.flag("sshEnabled") == Some(true) {
        let (ssh, kind) = ssh(ctx)?;
        spec.ssh = Some(ssh);
        secrets.extend(kind);
    }
    Ok((spec, secrets))
}

fn build_sqlite(
    ctx: &mut Ctx<'_>,
    name: &str,
    driver: DriverId,
) -> Result<(ConnectionSpec, Vec<ProfileSecretKind>), SkipReason> {
    let raw = ctx
        .text("defaultDatabase")
        .filter(|t| !t.trim().is_empty())
        .or_else(|| ctx.text("databaseFile"))
        .unwrap_or_default();
    let path = foreign_path(&raw).map_err(|_| invalid("the database file path is not usable"))?;
    let mut spec = ConnectionSpec::new(name, driver, path.to_string_lossy().into_owned());
    colour(ctx, &mut spec);
    if ctx.flag("sshEnabled") == Some(true) || ctx.flag("ssl") == Some(true) {
        ctx.notes
            .push("SSH and TLS settings do not apply to a SQLite file and were ignored".to_owned());
    }
    Ok((spec, Vec::new()))
}

/// The host part of the target.
fn host_text(raw: &str) -> Result<String, SkipReason> {
    let host = raw.trim();
    if host.is_empty() {
        return Err(invalid("the host is missing"));
    }
    let inner = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    if let Ok(addr) = inner.parse::<Ipv6Addr>() {
        return Ok(format!("[{addr}]"));
    }
    if host
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '.' | '-' | '_'))
    {
        Ok(host.to_owned())
    } else {
        Err(invalid("the host is not valid"))
    }
}

fn colour(ctx: &mut Ctx<'_>, spec: &mut ConnectionSpec) {
    let Some(color) = ctx.text("labelColor") else {
        return;
    };
    let c = color.trim().to_ascii_lowercase();
    if c.is_empty() || c == "default" {
        return;
    }
    if c.len() == 7 && c.starts_with('#') && c[1..].bytes().all(|b| b.is_ascii_hexdigit()) {
        spec.color = Some(c);
        return;
    }
    let hex = match c.as_str() {
        "red" => "#e53e3e",
        "orange" => "#dd6b20",
        "yellow" => "#d69e2e",
        "green" => "#38a169",
        "blue" => "#3182ce",
        "purple" => "#805ad5",
        "pink" => "#d53f8c",
        "gray" | "grey" => "#718096",
        _ => {
            ctx.notes
                .push("the colour is not known and was ignored".to_owned());
            return;
        }
    };
    spec.color = Some(hex.to_owned());
}

fn tls(ctx: &mut Ctx<'_>, engine: Engine) -> Option<TlsProfile> {
    let ssl = ctx.flag("ssl")?;
    if !ssl {
        return Some(TlsProfile::new(TlsMode::Disable));
    }
    let reject = ctx.flag("sslRejectUnauthorized").unwrap_or(true);
    let mode = if reject {
        TlsMode::VerifyFull
    } else {
        TlsMode::Require
    };
    let mut profile = TlsProfile::new(mode);
    let ca = ctx.text("sslCaFile");
    let cert = ctx.text("sslCertFile");
    let key = ctx.text("sslKeyFile");
    profile.ca_cert = checked_path(ctx, "sslCaFile", ca);
    let cert = checked_path(ctx, "sslCertFile", cert);
    let key = checked_path(ctx, "sslKeyFile", key);
    match (cert, key) {
        (Some(c), Some(k)) if engine == Engine::SqlServer => {
            drop((c, k));
            ctx.notes
                .push("SQL Server does not use client certificates; they were dropped".to_owned());
        }
        (Some(c), Some(k)) => {
            profile.client_cert = Some(c);
            profile.client_key = Some(k);
        }
        (None, None) => {}
        _ => ctx.notes.push(
            "the client certificate and key must come as a pair; both were dropped".to_owned(),
        ),
    }
    Some(profile)
}

fn checked_path(ctx: &mut Ctx<'_>, field: &str, raw: Option<String>) -> Option<PathBuf> {
    let raw = raw.filter(|r| !r.trim().is_empty())?;
    match foreign_path(&raw) {
        Ok(p) => Some(p),
        Err(reason) => {
            ctx.notes.push(format!("{field} was dropped: {reason}"));
            None
        }
    }
}

fn ssh(ctx: &mut Ctx<'_>) -> Result<(SshProfile, Option<ProfileSecretKind>), SkipReason> {
    if ctx
        .text("sshBastionHost")
        .is_some_and(|b| !b.trim().is_empty())
    {
        return Err(SkipReason::Unsupported(
            "SSH bastion hosts are not supported".to_owned(),
        ));
    }
    let host = ctx.text("sshHost").unwrap_or_default();
    let host = host.trim();
    if host.is_empty() || host.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(invalid("the SSH tunnel host is missing or not valid"));
    }
    let user = ctx.text("sshUsername").unwrap_or_default();
    let user = user.trim();
    if user.is_empty() {
        return Err(invalid("the SSH tunnel user is missing"));
    }
    let port = ctx
        .port("sshPort", DEFAULT_SSH_PORT)
        .map_err(|()| invalid("the SSH tunnel port is not valid"))?;
    let mode = ctx.text("sshMode").unwrap_or_default();
    let (auth, kind) = match mode.trim().to_ascii_lowercase().as_str() {
        "" | "userpass" => (SshAuth::Password, Some(ProfileSecretKind::SshPassword)),
        "agent" => (SshAuth::Agent, None),
        "keyfile" => {
            let raw = ctx.text("sshKeyfile").unwrap_or_default();
            let path = foreign_path(&raw).map_err(|_| invalid("SSH key path not usable"))?;
            (
                SshAuth::KeyFile {
                    path,
                    passphrase_in_keychain: false,
                },
                Some(ProfileSecretKind::SshPassphrase),
            )
        }
        _ => return Err(invalid("the SSH tunnel authentication is not recognised")),
    };
    let mut profile = SshProfile::new(host, user, auth);
    profile.port = port;
    Ok((profile, kind))
}
