// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Maps one `servers.json` entry to a [`ConnectionSpec`].

use std::fmt::Write as _;
use std::net::Ipv6Addr;
use std::path::PathBuf;

use serde_json::{Map, Value};
use sqler_config::{ConnectionSpec, ProfileSecretKind, SshAuth, SshProfile, TlsMode, TlsProfile};
use sqler_core::DriverId;

use crate::MAX_FIELD_CHARS;
use crate::model::{EntryOutcome, SkipReason, SourceEntry};
use crate::path::foreign_path;

const DRIVER: &str = "postgres";
const DEFAULT_PORT: u16 = 5432;
const DEFAULT_SSH_PORT: u16 = 22;
const MAX_NAME_SHOWN: usize = 64;

/// Top-level keys that are mapped (or are placeholders the tool always writes for the mapped
/// features). Every other key is reported by name as unsupported.
const MAPPED: [&str; 19] = [
    "Name",
    "Group",
    "Host",
    "Port",
    "MaintenanceDB",
    "Username",
    "SSLMode",
    "SSLRootCert",
    "SSLCert",
    "SSLKey",
    "ConnectionParameters",
    "BGColor",
    "UseSSHTunnel",
    "TunnelHost",
    "TunnelPort",
    "TunnelUsername",
    "TunnelAuthentication",
    "TunnelIdentityFile",
    "TunnelIdentityfile",
];
/// Keys of `ConnectionParameters` that are mapped.
const MAPPED_PARAMS: [&str; 4] = ["sslmode", "sslrootcert", "sslcert", "sslkey"];

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

struct Ctx<'a> {
    obj: &'a Map<String, Value>,
    notes: Vec<String>,
}

impl Ctx<'_> {
    /// A string field: `None` when absent, null, of the wrong type or too long (with a note).
    fn text(&mut self, key: &str) -> Option<String> {
        let value = self.obj.get(key)?;
        self.text_of(key, value)
    }

    fn text_of(&mut self, key: &str, value: &Value) -> Option<String> {
        match value {
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
    fn number(&mut self, key: &str) -> Result<Option<u64>, ()> {
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

    fn flag(&mut self, key: &str) -> bool {
        match self.obj.get(key) {
            None | Some(Value::Null) => false,
            Some(Value::Bool(b)) => *b,
            Some(Value::Number(n)) => n.as_u64() == Some(1),
            Some(Value::String(s)) => {
                let t = s.trim().to_ascii_lowercase();
                t == "1" || t == "true"
            }
            Some(_) => {
                self.notes.push(format!(
                    "field {} has an unexpected type and was ignored",
                    safe_name(key)
                ));
                false
            }
        }
    }
}

fn port(ctx: &mut Ctx<'_>, key: &str, default: u16) -> Result<u16, ()> {
    match ctx.number(key)? {
        None => Ok(default),
        Some(n) => u16::try_from(n).ok().filter(|p| *p != 0).ok_or(()),
    }
}

fn invalid(text: &str) -> SkipReason {
    SkipReason::Invalid(text.to_owned())
}

pub(super) fn map_entry(index: usize, value: &Value) -> SourceEntry {
    let fallback = format!("pgAdmin server {}", index + 1);
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
        .text("Name")
        .map(|n| clean_name(&n))
        .filter(|n| !n.is_empty())
        .unwrap_or(fallback);
    let group = ctx
        .text("Group")
        .map(|g| clean_name(&g))
        .filter(|g| !g.is_empty())
        .into_iter()
        .collect();

    let mut unsupported: Vec<String> = obj
        .keys()
        .filter(|k| !MAPPED.contains(&k.as_str()))
        .map(|k| safe_name(k))
        .collect();
    if let Some(Value::Object(params)) = obj.get("ConnectionParameters") {
        unsupported.extend(
            params
                .keys()
                .filter(|k| !MAPPED_PARAMS.contains(&k.as_str()))
                .map(|k| format!("ConnectionParameters.{}", safe_name(k))),
        );
    }

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

fn build(
    ctx: &mut Ctx<'_>,
    name: &str,
) -> Result<(ConnectionSpec, Vec<ProfileSecretKind>), SkipReason> {
    let driver = DriverId::new(DRIVER).map_err(|_| invalid("driver id"))?;
    let host_raw = ctx.text("Host").unwrap_or_default();
    let host = host_text(&host_raw);
    let host = match host {
        Ok(h) => h,
        Err(reason) => {
            let service = ctx.text("Service").unwrap_or_default();
            if host_raw.trim().is_empty() && !service.trim().is_empty() {
                return Err(SkipReason::Unsupported(
                    "pg_service entries are not supported".to_owned(),
                ));
            }
            return Err(reason);
        }
    };
    let port = port(ctx, "Port", DEFAULT_PORT).map_err(|()| invalid("the port is not valid"))?;
    let db = ctx
        .text("MaintenanceDB")
        .map(|d| d.trim().to_owned())
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| "postgres".to_owned());
    let user = ctx
        .text("Username")
        .map(|u| u.trim().to_owned())
        .unwrap_or_default();

    let mut target = format!("postgres://{host}:{port}/{}", encode(&db));
    if !user.is_empty() {
        let _ = write!(target, "?user={}", encode(&user));
    }
    let mut spec = ConnectionSpec::new(name, driver, target);

    if let Some(tls) = tls(ctx) {
        spec.tls = Some(tls);
    }
    if let Some(color) = ctx.text("BGColor") {
        let c = color.trim();
        if c.len() == 7 && c.starts_with('#') && c[1..].bytes().all(|b| b.is_ascii_hexdigit()) {
            spec.color = Some(c.to_ascii_lowercase());
        } else if !c.is_empty() {
            ctx.notes
                .push("the colour is not #rrggbb and was ignored".to_owned());
        }
    }

    let mut secrets = Vec::new();
    let kerberos = ctx.flag("KerberosAuthentication");
    if !user.is_empty() && !kerberos {
        secrets.push(ProfileSecretKind::DbPassword);
    }
    if ctx.flag("UseSSHTunnel") {
        let (ssh, kind) = ssh(ctx)?;
        spec.ssh = Some(ssh);
        secrets.push(kind);
    }
    Ok((spec, secrets))
}

/// The host part of the target.
fn host_text(raw: &str) -> Result<String, SkipReason> {
    let host = raw.trim();
    if host.is_empty() {
        return Err(invalid("the host is missing"));
    }
    if host
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '@' | '?' | '#'))
    {
        return Err(invalid("the host is not valid"));
    }
    if host.starts_with('/') {
        return Ok(encode(host));
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

/// Reads a TLS setting from `ConnectionParameters`, then from the legacy top-level key.
fn tls_value(ctx: &mut Ctx<'_>, param: &str, legacy: &str) -> Option<String> {
    let from_params = match ctx.obj.get("ConnectionParameters") {
        Some(Value::Object(p)) => p.get(param).cloned(),
        _ => None,
    };
    let from_params = from_params.and_then(|v| ctx.text_of(param, &v));
    from_params
        .filter(|s| !s.trim().is_empty())
        .or_else(|| ctx.text(legacy).filter(|s| !s.trim().is_empty()))
        .map(|s| s.trim().to_owned())
}

fn tls(ctx: &mut Ctx<'_>) -> Option<TlsProfile> {
    if matches!(ctx.obj.get("ConnectionParameters"), Some(v) if !v.is_object() && !v.is_null()) {
        ctx.notes.push(
            "field \"ConnectionParameters\" has an unexpected type and was ignored".to_owned(),
        );
    }
    let mode_text = tls_value(ctx, "sslmode", "SSLMode");
    let root = tls_value(ctx, "sslrootcert", "SSLRootCert");
    let cert = tls_value(ctx, "sslcert", "SSLCert");
    let key = tls_value(ctx, "sslkey", "SSLKey");
    let has_files = root.is_some() || cert.is_some() || key.is_some();

    let mode = match mode_text.as_deref().map(str::to_ascii_lowercase).as_deref() {
        None => None,
        Some("allow") => {
            ctx.notes
                .push("sslmode \"allow\" was mapped to \"prefer\"".to_owned());
            Some(TlsMode::Prefer)
        }
        Some(other) => {
            let mode = TlsMode::from_id(other);
            if mode.is_none() {
                ctx.notes
                    .push("the sslmode value is not known and was ignored".to_owned());
            }
            mode
        }
    };
    let Some(mode) = mode else {
        if has_files {
            ctx.notes
                .push("TLS files were ignored because no usable sslmode is set".to_owned());
        }
        return None;
    };

    let mut profile = TlsProfile::new(mode);
    profile.ca_cert = checked_path(ctx, "sslrootcert", root);
    let cert = checked_path(ctx, "sslcert", cert);
    let key = checked_path(ctx, "sslkey", key);
    match (cert, key) {
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
    let raw = raw?;
    match foreign_path(&raw) {
        Ok(p) => Some(p),
        Err(reason) => {
            ctx.notes.push(format!("{field} was dropped: {reason}"));
            None
        }
    }
}

fn ssh(ctx: &mut Ctx<'_>) -> Result<(SshProfile, ProfileSecretKind), SkipReason> {
    let host = ctx.text("TunnelHost").unwrap_or_default();
    let host = host.trim();
    if host.is_empty() || host.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(invalid("the SSH tunnel host is missing or not valid"));
    }
    let user = ctx.text("TunnelUsername").unwrap_or_default();
    let user = user.trim();
    if user.is_empty() {
        return Err(invalid("the SSH tunnel user is missing"));
    }
    let port = port(ctx, "TunnelPort", DEFAULT_SSH_PORT)
        .map_err(|()| invalid("the SSH tunnel port is not valid"))?;
    let (auth, kind) = match ctx.number("TunnelAuthentication") {
        Ok(None | Some(0)) => (SshAuth::Password, ProfileSecretKind::SshPassword),
        Ok(Some(1)) => {
            let raw = ctx
                .text("TunnelIdentityFile")
                .or_else(|| ctx.text("TunnelIdentityfile"))
                .unwrap_or_default();
            let path = foreign_path(&raw).map_err(|_| invalid("SSH key path not usable"))?;
            (
                SshAuth::KeyFile {
                    path,
                    passphrase_in_keychain: false,
                },
                ProfileSecretKind::SshPassphrase,
            )
        }
        _ => return Err(invalid("the SSH tunnel authentication is not recognised")),
    };
    let mut profile = SshProfile::new(host, user, auth);
    profile.port = port;
    Ok((profile, kind))
}
