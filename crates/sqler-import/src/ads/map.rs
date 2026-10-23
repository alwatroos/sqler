// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Maps one `datasource.connections` entry to a [`ConnectionSpec`].
//!
//! The stored `password` option is never read for its value: only its presence is noted.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::net::Ipv6Addr;

use serde_json::{Map, Value};
use sqler_config::{ConnectionSpec, ProfileSecretKind, TlsMode, TlsProfile};
use sqler_core::DriverId;

use crate::model::{EntryOutcome, SkipReason, SourceEntry};
use crate::{MAX_FIELD_CHARS, MAX_GROUP_DEPTH};

const MSSQL: &str = "sqlserver";
const PGSQL: &str = "postgres";
const MSSQL_PORT: u16 = 1433;
const PGSQL_PORT: u16 = 5432;
const MAX_NAME_SHOWN: usize = 64;
/// Most parent links followed when resolving a group path (cycle guard).
const MAX_HOPS: usize = 64;
const ROOT_NAME: &str = "ROOT";

/// Top-level keys of an entry that are mapped or carry no setting.
const TOP_MAPPED: [&str; 5] = ["options", "groupId", "providerName", "savedProfile", "id"];
/// Option keys that are mapped for every provider (`password` is only noted, never read).
const OPTION_MAPPED: [&str; 9] = [
    "connectionName",
    "server",
    "database",
    "authenticationType",
    "user",
    "groupId",
    "password",
    "applicationName",
    "databaseDisplayName",
];

/// A foreign key or value made safe to show: control characters replaced, cut, quoted.
fn safe_name(key: &str) -> String {
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

fn clean_name(raw: &str) -> String {
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

fn hex_color(raw: &str) -> Option<String> {
    let c = raw.trim();
    (c.len() == 7 && c.starts_with('#') && c[1..].bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| c.to_ascii_lowercase())
}

/// A connection group of `datasource.connectionGroups`.
#[derive(Debug, Clone)]
pub(super) struct Group {
    name: String,
    parent: Option<String>,
    color: Option<String>,
}

impl Group {
    /// The invisible group at the top of the tree.
    fn is_root(&self) -> bool {
        self.parent.is_none() && self.name == ROOT_NAME
    }
}

/// One group item: its id and the group. `None` when the id or the name is unusable.
pub(super) fn group(item: &Value) -> Option<(String, Group)> {
    let obj = item.as_object()?;
    let short = |v: Option<&Value>| match v {
        Some(Value::String(s)) if s.chars().count() <= MAX_FIELD_CHARS => Some(s.trim().to_owned()),
        _ => None,
    };
    let id = short(obj.get("id")).filter(|s| !s.is_empty())?;
    let name = short(obj.get("name")).map(|n| clean_name(&n))?;
    if name.is_empty() {
        return None;
    }
    let parent = short(obj.get("parentId")).filter(|s| !s.is_empty());
    let color = short(obj.get("color")).and_then(|c| hex_color(&c));
    Some((
        id,
        Group {
            name,
            parent,
            color,
        },
    ))
}

/// The group path (outermost first) and the nearest group colour for a group id.
fn resolve_group(
    groups: &HashMap<String, Group>,
    id: &str,
    notes: &mut Vec<String>,
) -> (Vec<String>, Option<String>) {
    let mut path = Vec::new();
    let mut color = None;
    let mut cur = Some(id.to_owned());
    let mut hops = 0;
    while let Some(key) = cur.take() {
        let Some(g) = groups.get(&key) else {
            if hops == 0 {
                notes.push(
                    "the connection group was not found; it is imported at the top".to_owned(),
                );
            }
            break;
        };
        if g.is_root() {
            break;
        }
        hops += 1;
        if hops > MAX_HOPS {
            notes.push(
                "the group nesting is cyclic or too deep; the connection is imported at the top"
                    .to_owned(),
            );
            path.clear();
            color = None;
            break;
        }
        if color.is_none() {
            color.clone_from(&g.color);
        }
        path.push(g.name.clone());
        cur.clone_from(&g.parent);
    }
    path.reverse();
    if path.len() > MAX_GROUP_DEPTH {
        path.truncate(MAX_GROUP_DEPTH);
        notes.push(format!(
            "the group path is deeper than {MAX_GROUP_DEPTH} levels; deeper levels were folded into the last one"
        ));
    }
    (path, color)
}

struct Ctx<'a> {
    top: &'a Map<String, Value>,
    opts: &'a Map<String, Value>,
    notes: Vec<String>,
}

impl Ctx<'_> {
    /// A string field of the options: `None` when absent, null, of the wrong type or too long.
    fn text(&mut self, key: &str) -> Option<String> {
        match self.opts.get(key)? {
            Value::Null => None,
            Value::String(s) if s.chars().count() > MAX_FIELD_CHARS => {
                self.notes.push(format!(
                    "option {} is too long and was ignored",
                    safe_name(key)
                ));
                None
            }
            Value::String(s) => Some(s.clone()),
            _ => {
                self.notes.push(format!(
                    "option {} has an unexpected type and was ignored",
                    safe_name(key)
                ));
                None
            }
        }
    }

    fn top_text(&self, key: &str) -> Option<String> {
        match self.top.get(key)? {
            Value::String(s) if s.chars().count() <= MAX_FIELD_CHARS => Some(s.clone()),
            _ => None,
        }
    }

    /// A port given as a JSON number or a digit string.
    fn port(&mut self, default: u16) -> Result<u16, ()> {
        let n = match self.opts.get("port") {
            None | Some(Value::Null) => return Ok(default),
            Some(Value::Number(n)) => n.as_u64().ok_or(())?,
            Some(Value::String(s)) if s.trim().is_empty() => return Ok(default),
            Some(Value::String(s)) => {
                let t = s.trim();
                if t.len() <= 10 && t.bytes().all(|b| b.is_ascii_digit()) {
                    t.parse().map_err(|_| ())?
                } else {
                    return Err(());
                }
            }
            Some(_) => return Err(()),
        };
        u16::try_from(n).ok().filter(|p| *p != 0).ok_or(())
    }
}

fn invalid(text: &str) -> SkipReason {
    SkipReason::Invalid(text.to_owned())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Provider {
    Mssql,
    Pgsql,
}

pub(super) fn map_entry(
    index: usize,
    value: &Value,
    groups: &HashMap<String, Group>,
) -> SourceEntry {
    let fallback = format!("Azure Data Studio connection {}", index + 1);
    let skipped = |name: String, reason: &str| SourceEntry {
        index,
        name,
        group: Vec::new(),
        outcome: EntryOutcome::Skipped(invalid(reason)),
        unsupported: Vec::new(),
        notes: Vec::new(),
        secrets: Vec::new(),
    };
    let Some(top) = value.as_object() else {
        return skipped(fallback, "the entry is not an object");
    };
    let Some(opts) = top.get("options").and_then(Value::as_object) else {
        return skipped(fallback, "the entry has no options");
    };
    let mut ctx = Ctx {
        top,
        opts,
        notes: Vec::new(),
    };

    let provider_text = ctx.top_text("providerName").unwrap_or_default();
    let provider = match provider_text.trim().to_ascii_uppercase().as_str() {
        "MSSQL" => Some(Provider::Mssql),
        "PGSQL" => Some(Provider::Pgsql),
        _ => None,
    };

    let name = ctx
        .text("connectionName")
        .map(|n| clean_name(&n))
        .filter(|n| !n.is_empty())
        .or_else(|| {
            ctx.text("server")
                .map(|s| {
                    clean_name(&s)
                        .chars()
                        .take(MAX_NAME_SHOWN)
                        .collect::<String>()
                })
                .filter(|n| !n.is_empty())
        })
        .unwrap_or(fallback);

    let group_id = ctx
        .top_text("groupId")
        .or_else(|| ctx.text("groupId"))
        .map(|g| g.trim().to_owned())
        .filter(|g| !g.is_empty());
    let (group_path, color) = match group_id {
        Some(id) => resolve_group(groups, &id, &mut ctx.notes),
        None => (Vec::new(), None),
    };

    let unsupported = unsupported_names(top, opts, provider);
    if matches!(opts.get("password"), Some(Value::String(s)) if !s.is_empty()) {
        ctx.notes
            .push("a stored password was not imported".to_owned());
    }

    let (outcome, secrets) = match provider {
        None => {
            if provider_text.trim().is_empty() {
                (
                    EntryOutcome::Skipped(invalid("the provider is missing")),
                    Vec::new(),
                )
            } else {
                ctx.notes
                    .push(format!("provider {}", safe_name(provider_text.trim())));
                (
                    EntryOutcome::Skipped(SkipReason::Unsupported(
                        "only SQL Server and PostgreSQL connections are imported".to_owned(),
                    )),
                    Vec::new(),
                )
            }
        }
        Some(p) => match build(&mut ctx, p, &name, color) {
            Ok((spec, secrets)) => (EntryOutcome::Mapped(spec), secrets),
            Err(reason) => (EntryOutcome::Skipped(reason), Vec::new()),
        },
    };
    // `server` is read for the name and again for the host: keep each note once.
    let mut seen = std::collections::HashSet::new();
    ctx.notes.retain(|n| seen.insert(n.clone()));
    SourceEntry {
        index,
        name,
        group: group_path,
        outcome,
        unsupported,
        notes: ctx.notes,
        secrets,
    }
}

/// Names of the keys that are not mapped, never secret-looking ones, never values.
fn unsupported_names(
    top: &Map<String, Value>,
    opts: &Map<String, Value>,
    provider: Option<Provider>,
) -> Vec<String> {
    let mut unsupported: Vec<String> = top
        .keys()
        .filter(|k| !TOP_MAPPED.contains(&k.as_str()) && !is_secret_name(k))
        .map(|k| safe_name(k))
        .collect();
    unsupported.extend(
        opts.keys()
            .filter(|k| {
                !OPTION_MAPPED.contains(&k.as_str())
                    && !provider_option(provider, k)
                    && !is_secret_name(k)
            })
            .map(|k| format!("options.{}", safe_name(k))),
    );
    unsupported
}

/// Keys whose name says they hold a secret; they are not even listed as unsupported.
fn is_secret_name(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    [
        "password",
        "passphrase",
        "pwd",
        "secret",
        "token",
        "apikey",
        "api_key",
        "accesskey",
        "privatekey",
        "sslkey",
        "credential",
    ]
    .iter()
    .any(|s| k.contains(s))
}

/// Option keys that only some providers map.
fn provider_option(provider: Option<Provider>, key: &str) -> bool {
    match provider {
        Some(Provider::Mssql) => matches!(key, "encrypt" | "trustServerCertificate"),
        Some(Provider::Pgsql) => key == "port",
        None => false,
    }
}

fn build(
    ctx: &mut Ctx<'_>,
    provider: Provider,
    name: &str,
    color: Option<String>,
) -> Result<(ConnectionSpec, Vec<ProfileSecretKind>), SkipReason> {
    let user = ctx
        .text("user")
        .map(|u| u.trim().to_owned())
        .unwrap_or_default();
    check_auth(ctx, !user.is_empty())?;
    if user.is_empty() {
        return Err(invalid("the SQL login user is missing"));
    }

    let server = ctx.text("server").unwrap_or_default();
    let (driver_id, host, port, default_db) = match provider {
        Provider::Mssql => {
            let (host, port) = mssql_server(&server)?;
            (MSSQL, host, port, "")
        }
        Provider::Pgsql => {
            let host = host_text(server.trim())?;
            let port = ctx
                .port(PGSQL_PORT)
                .map_err(|()| invalid("the port is not valid"))?;
            (PGSQL, host, port, "postgres")
        }
    };
    let driver = DriverId::new(driver_id).map_err(|_| invalid("driver id"))?;
    let db = ctx
        .text("database")
        .map(|d| d.trim().to_owned())
        .filter(|d| !d.is_empty() && !d.eq_ignore_ascii_case("<default>"))
        .unwrap_or_else(|| default_db.to_owned());

    let mut target = format!("{driver_id}://{host}:{port}");
    if !db.is_empty() {
        let _ = write!(target, "/{}", encode(&db));
    }
    let _ = write!(target, "?user={}", encode(&user));
    let mut spec = ConnectionSpec::new(name, driver, target);
    spec.color = color;
    if provider == Provider::Mssql {
        spec.tls = tls(ctx);
    }
    Ok((spec, vec![ProfileSecretKind::DbPassword]))
}

/// Only SQL login is imported (ADR-0071). A missing type counts as SQL login when a user is set.
fn check_auth(ctx: &mut Ctx<'_>, has_user: bool) -> Result<(), SkipReason> {
    let kind = ctx.text("authenticationType").unwrap_or_default();
    let kind = kind.trim();
    if kind.is_empty() {
        return if has_user {
            Ok(())
        } else {
            Err(invalid("the authentication type is missing"))
        };
    }
    match kind.to_ascii_lowercase().as_str() {
        "sqllogin" | "password" => Ok(()),
        "integrated" => Err(SkipReason::Unsupported(
            "Windows (integrated) authentication is not supported; SQL login only".to_owned(),
        )),
        "azuremfa"
        | "azurepassword"
        | "activedirectorypassword"
        | "activedirectoryintegrated"
        | "activedirectoryinteractive"
        | "activedirectoryserviceprincipal"
        | "activedirectorydefault" => Err(SkipReason::Unsupported(
            "Azure Active Directory authentication is not supported; SQL login only".to_owned(),
        )),
        _ => Err(SkipReason::Unsupported(
            "this authentication type is not supported; SQL login only".to_owned(),
        )),
    }
}

/// `host`, `host,port`, `tcp:host,port` or `[v6],port` of a SQL Server `server` option.
fn mssql_server(raw: &str) -> Result<(String, u16), SkipReason> {
    let mut s = raw.trim();
    if s.get(..4).is_some_and(|p| p.eq_ignore_ascii_case("tcp:")) {
        s = s.get(4..).unwrap_or_default().trim_start();
    }
    if s.contains('\\') {
        return Err(SkipReason::Unsupported(
            "named SQL Server instances are not supported; use the host and port".to_owned(),
        ));
    }
    let (host_part, port_part) = if let Some(rest) = s.strip_prefix('[') {
        match rest.split_once(']') {
            Some((h, tail)) => (format!("[{h}]"), tail.strip_prefix(',')),
            None => return Err(invalid("the host is not valid")),
        }
    } else {
        match s.rsplit_once(',') {
            Some((h, p)) => (h.to_owned(), Some(p)),
            None => (s.to_owned(), None),
        }
    };
    let port = match port_part.map(str::trim) {
        None => MSSQL_PORT,
        Some(p) if !p.is_empty() && p.len() <= 5 && p.bytes().all(|b| b.is_ascii_digit()) => p
            .parse::<u16>()
            .ok()
            .filter(|n| *n != 0)
            .ok_or_else(|| invalid("the port is not valid"))?,
        Some(_) => return Err(invalid("the port is not valid")),
    };
    Ok((host_text(host_part.trim())?, port))
}

/// The host part of the target.
fn host_text(host: &str) -> Result<String, SkipReason> {
    if host.is_empty() {
        return Err(invalid("the host is missing"));
    }
    if host == "." || host.eq_ignore_ascii_case("(local)") {
        return Ok("localhost".to_owned());
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Encrypt {
    Off,
    On,
    Strict,
}

fn encrypt_of(v: &Value) -> Option<Encrypt> {
    match v {
        Value::Bool(true) => Some(Encrypt::On),
        Value::Bool(false) => Some(Encrypt::Off),
        Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
            "true" | "yes" | "mandatory" | "1" => Some(Encrypt::On),
            "false" | "no" | "optional" | "0" => Some(Encrypt::Off),
            "strict" => Some(Encrypt::Strict),
            _ => None,
        },
        _ => None,
    }
}

fn flag_of(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_u64() == Some(1),
        Some(Value::String(s)) => {
            let t = s.trim().to_ascii_lowercase();
            t == "1" || t == "true" || t == "yes"
        }
        _ => false,
    }
}

/// `encrypt` and `trustServerCertificate` to a TLS profile (drivers.md "SQL Server"). No `encrypt`
/// option means no profile, which the driver reads as `prefer`.
fn tls(ctx: &mut Ctx<'_>) -> Option<TlsProfile> {
    let raw = match ctx.opts.get("encrypt") {
        None | Some(Value::Null) => return None,
        Some(v) => v,
    };
    let trust = flag_of(ctx.opts.get("trustServerCertificate"));
    let Some(encrypt) = encrypt_of(raw) else {
        ctx.notes
            .push("the encrypt value is not known and was ignored".to_owned());
        return None;
    };
    let mode = match encrypt {
        Encrypt::Off => {
            ctx.notes.push(
                "encrypt \"off\" was mapped to \"prefer\" (encrypted, certificate not checked)"
                    .to_owned(),
            );
            TlsMode::Prefer
        }
        Encrypt::On if trust => TlsMode::Require,
        Encrypt::On => TlsMode::VerifyFull,
        Encrypt::Strict => {
            if trust {
                ctx.notes.push(
                    "trustServerCertificate was ignored because encrypt is \"strict\"".to_owned(),
                );
            }
            TlsMode::VerifyFull
        }
    };
    if mode == TlsMode::VerifyFull && ctx.opts.contains_key("hostNameInCertificate") {
        ctx.notes
            .push("hostNameInCertificate is not mapped; the host name is checked".to_owned());
    }
    Some(TlsProfile::new(mode))
}
