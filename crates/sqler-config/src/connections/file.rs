// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `connections.toml` wire formats: v0 (M1, unversioned) and v1, with
//! version dispatch, migration and serialisation.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sqler_core::{DriverId, Environment};

use super::model::{
    CONNECTIONS_FORMAT_VERSION, ConnectionId, ConnectionProfile, ConnectionSpec, Group, GroupId,
    SshAuth, SshProfile, TlsMode, TlsProfile, is_valid_id, unique_slug, validate_spec,
};
use super::parse_message;
use super::store::ConnectionStore;
use crate::ConfigError;

// ---- wire structs -------------------------------------------------------

/// The M1 format (ADR-0005): no version key, a flat connection list.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct V0File {
    #[serde(default, rename = "connection")]
    connections: Vec<Entry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    name: String,
    driver: String,
    target: String,
    color: Option<String>,
    environment: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct V1File {
    version: u32,
    #[serde(default, rename = "group", skip_serializing_if = "Vec::is_empty")]
    groups: Vec<WireGroup>,
    #[serde(default, rename = "connection", skip_serializing_if = "Vec::is_empty")]
    connections: Vec<WireConnection>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireGroup {
    id: String,
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireConnection {
    id: String,
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    group: Option<String>,
    driver: String,
    target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    environment: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    password_in_keychain: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tls: Option<WireTls>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ssh: Option<WireSsh>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireTls {
    mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ca_cert: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    client_cert: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    client_key: Option<PathBuf>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireSsh {
    host: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    port: Option<u16>,
    user: String,
    auth: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    key_file: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "is_false")]
    passphrase_in_keychain: bool,
}

// serde's `skip_serializing_if` passes a reference.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(b: &bool) -> bool {
    !*b
}

// ---- loading ------------------------------------------------------------

fn invalid(path: &Path, what: &str, i: usize, name: &str, msg: &str) -> ConfigError {
    ConfigError::Invalid {
        path: path.to_owned(),
        message: format!("{what}[{i}] \"{name}\": {msg}"),
    }
}

fn plain_invalid(path: &Path, message: String) -> ConfigError {
    ConfigError::Invalid {
        path: path.to_owned(),
        message,
    }
}

/// Parses the text of `connections.toml`, migrating the M1 format in memory.
pub(super) fn parse(path: &Path, text: &str) -> Result<ConnectionStore, ConfigError> {
    let parse_err = |e: &toml::de::Error| ConfigError::Parse {
        path: path.to_owned(),
        message: parse_message(text, e),
    };
    let table: toml::Table = toml::from_str(text).map_err(|e| parse_err(&e))?;
    let version = match table.get("version") {
        None => None,
        Some(toml::Value::Integer(n)) if *n >= 0 => Some(*n),
        Some(_) => {
            return Err(ConfigError::Parse {
                path: path.to_owned(),
                message: "version must be a non-negative integer".to_owned(),
            });
        }
    };
    match version {
        None => {
            let file: V0File = toml::from_str(text).map_err(|e| parse_err(&e))?;
            migrate_v0(path, &file)
        }
        Some(n) if n == i64::from(CONNECTIONS_FORMAT_VERSION) => {
            let file: V1File = toml::from_str(text).map_err(|e| parse_err(&e))?;
            convert_v1(path, &file)
        }
        Some(0) => Err(plain_invalid(
            path,
            "unsupported version 0 (the first versioned format is 1)".to_owned(),
        )),
        Some(n) => Err(plain_invalid(
            path,
            format!(
                "written by a newer SQLer (version {n}, this build reads up to {CONNECTIONS_FORMAT_VERSION})"
            ),
        )),
    }
}

fn driver_and_env(
    driver: &str,
    environment: Option<&str>,
) -> Result<(DriverId, Option<Environment>), String> {
    let driver = DriverId::new(driver).map_err(|e| e.to_string())?;
    let env = environment
        .map(|env| {
            Environment::from_id(env)
                .ok_or_else(|| format!("environment {env:?} is not one of: dev, staging, prod"))
        })
        .transpose()?;
    Ok((driver, env))
}

fn base_spec(
    name: &str,
    driver: &str,
    target: &str,
    color: Option<&str>,
    environment: Option<&str>,
) -> Result<ConnectionSpec, String> {
    let (driver, env) = driver_and_env(driver, environment)?;
    let mut spec = ConnectionSpec::new(name, driver, target);
    spec.color = color.map(str::to_owned);
    spec.environment = env;
    Ok(spec)
}

fn migrate_v0(path: &Path, file: &V0File) -> Result<ConnectionStore, ConfigError> {
    let mut names = HashSet::new();
    let mut ids: HashSet<String> = HashSet::new();
    let mut connections = Vec::with_capacity(file.connections.len());
    for (i, e) in file.connections.iter().enumerate() {
        let fail = |msg: &str| invalid(path, "connection", i, &e.name, msg);
        let spec = base_spec(
            &e.name,
            &e.driver,
            &e.target,
            e.color.as_deref(),
            e.environment.as_deref(),
        )
        .map_err(|m| fail(&m))?;
        let spec = validate_spec(spec).map_err(|err| fail(&err.to_string()))?;
        if !names.insert(spec.name.clone()) {
            return Err(fail("duplicate connection name"));
        }
        let id = unique_slug(&spec.name, "connection", |s| ids.contains(s));
        ids.insert(id.clone());
        connections.push(ConnectionProfile::new(
            ConnectionId::from_valid(id),
            None,
            spec,
        ));
    }
    Ok(ConnectionStore::from_parts(Vec::new(), connections))
}

fn check_id(id: &str) -> Result<(), String> {
    if is_valid_id(id) {
        Ok(())
    } else {
        Err(format!(
            "id {id:?} must be non-empty lowercase letters, digits and `-`"
        ))
    }
}

fn convert_v1(path: &Path, file: &V1File) -> Result<ConnectionStore, ConfigError> {
    let groups = convert_groups(path, &file.groups)?;
    let group_ids: HashSet<&str> = groups.iter().map(|g| g.id().as_str()).collect();
    let mut ids = HashSet::new();
    let mut names = HashSet::new();
    let mut connections = Vec::with_capacity(file.connections.len());
    for (i, c) in file.connections.iter().enumerate() {
        let fail = |msg: &str| invalid(path, "connection", i, &c.name, msg);
        check_id(&c.id).map_err(|m| fail(&m))?;
        if !ids.insert(c.id.as_str()) {
            return Err(fail("duplicate connection id"));
        }
        if let Some(g) = &c.group
            && !group_ids.contains(g.as_str())
        {
            return Err(fail(&format!("group {g:?} does not exist")));
        }
        let spec = spec_from_wire(c).map_err(|m| fail(&m))?;
        let spec = validate_spec(spec).map_err(|e| fail(&e.to_string()))?;
        if !names.insert(spec.name.clone()) {
            return Err(fail("duplicate connection name"));
        }
        connections.push(ConnectionProfile::new(
            ConnectionId::from_valid(c.id.clone()),
            c.group.clone().map(GroupId::from_valid),
            spec,
        ));
    }
    Ok(ConnectionStore::from_parts(groups, connections))
}

fn convert_groups(path: &Path, wire: &[WireGroup]) -> Result<Vec<Group>, ConfigError> {
    let mut ids = HashSet::new();
    for (i, g) in wire.iter().enumerate() {
        let fail = |msg: &str| invalid(path, "group", i, &g.name, msg);
        check_id(&g.id).map_err(|m| fail(&m))?;
        if !ids.insert(g.id.as_str()) {
            return Err(fail("duplicate group id"));
        }
        if g.name.trim().is_empty() {
            return Err(fail("name must not be empty"));
        }
    }
    let mut sibling_names = HashSet::new();
    for (i, g) in wire.iter().enumerate() {
        let fail = |msg: &str| invalid(path, "group", i, &g.name, msg);
        if let Some(p) = &g.parent {
            if !ids.contains(p.as_str()) {
                return Err(fail(&format!("parent {p:?} does not exist")));
            }
            // A chain longer than the number of groups must loop.
            let mut cursor = Some(p.as_str());
            let mut steps = 0;
            while let Some(cur) = cursor {
                if cur == g.id || steps > wire.len() {
                    return Err(fail("parent chain forms a cycle"));
                }
                steps += 1;
                cursor = wire
                    .iter()
                    .find(|x| x.id == cur)
                    .and_then(|x| x.parent.as_deref());
            }
        }
        if !sibling_names.insert((g.parent.as_deref(), g.name.trim())) {
            return Err(fail("duplicate group name among siblings"));
        }
    }
    Ok(wire
        .iter()
        .map(|g| {
            Group::new(
                GroupId::from_valid(g.id.clone()),
                g.name.trim().to_owned(),
                g.parent.clone().map(GroupId::from_valid),
            )
        })
        .collect())
}

fn spec_from_wire(c: &WireConnection) -> Result<ConnectionSpec, String> {
    let mut spec = base_spec(
        &c.name,
        &c.driver,
        &c.target,
        c.color.as_deref(),
        c.environment.as_deref(),
    )?;
    spec.password_in_keychain = c.password_in_keychain;
    if let Some(t) = &c.tls {
        let mode = TlsMode::from_id(&t.mode).ok_or_else(|| {
            format!(
                "tls mode {:?} is not one of: disable, prefer, require, verify-ca, verify-full",
                t.mode
            )
        })?;
        let mut tls = TlsProfile::new(mode);
        tls.ca_cert.clone_from(&t.ca_cert);
        tls.client_cert.clone_from(&t.client_cert);
        tls.client_key.clone_from(&t.client_key);
        spec.tls = Some(tls);
    }
    if let Some(s) = &c.ssh {
        spec.ssh = Some(ssh_from_wire(s)?);
    }
    Ok(spec)
}

fn ssh_from_wire(s: &WireSsh) -> Result<SshProfile, String> {
    let auth = match (s.auth.as_str(), &s.key_file) {
        ("password", None) => SshAuth::Password,
        ("agent", None) => SshAuth::Agent,
        ("key-file", Some(path)) => SshAuth::KeyFile {
            path: path.clone(),
            passphrase_in_keychain: s.passphrase_in_keychain,
        },
        ("key-file", None) => {
            return Err("invalid SSH settings: auth \"key-file\" requires key_file".to_owned());
        }
        ("password" | "agent", Some(_)) => {
            return Err(
                "invalid SSH settings: key_file is only valid with auth = \"key-file\"".to_owned(),
            );
        }
        (other, _) => {
            return Err(format!(
                "invalid SSH settings: auth {other:?} is not one of: password, key-file, agent"
            ));
        }
    };
    if s.passphrase_in_keychain && !matches!(auth, SshAuth::KeyFile { .. }) {
        return Err(
            "invalid SSH settings: passphrase_in_keychain is only valid with auth = \"key-file\""
                .to_owned(),
        );
    }
    let mut ssh = SshProfile::new(s.host.clone(), s.user.clone(), auth);
    ssh.port = s.port.unwrap_or(22);
    Ok(ssh)
}

// ---- saving -------------------------------------------------------------

/// Serialises the store as a `version = 1` file.
pub(super) fn serialize(path: &Path, store: &ConnectionStore) -> Result<String, ConfigError> {
    let file = V1File {
        version: CONNECTIONS_FORMAT_VERSION,
        groups: store
            .groups()
            .iter()
            .map(|g| WireGroup {
                id: g.id().to_string(),
                name: g.name().to_owned(),
                parent: g.parent().map(ToString::to_string),
            })
            .collect(),
        connections: store.connections().iter().map(wire_connection).collect(),
    };
    toml::to_string_pretty(&file).map_err(|e| ConfigError::Serialize {
        path: path.to_owned(),
        message: e.to_string(),
    })
}

fn wire_connection(p: &ConnectionProfile) -> WireConnection {
    let s = p.spec();
    WireConnection {
        id: p.id().to_string(),
        name: s.name.clone(),
        group: p.group().map(ToString::to_string),
        driver: s.driver.to_string(),
        target: s.target.clone(),
        color: s.color.clone(),
        environment: s.environment.map(|e| e.as_str().to_owned()),
        password_in_keychain: s.password_in_keychain,
        tls: s.tls.as_ref().map(|t| WireTls {
            mode: t.mode.as_str().to_owned(),
            ca_cert: t.ca_cert.clone(),
            client_cert: t.client_cert.clone(),
            client_key: t.client_key.clone(),
        }),
        ssh: s.ssh.as_ref().map(|h| {
            let (auth, key_file, passphrase) = match &h.auth {
                SshAuth::Password => ("password", None, false),
                SshAuth::Agent => ("agent", None, false),
                SshAuth::KeyFile {
                    path,
                    passphrase_in_keychain,
                } => ("key-file", Some(path.clone()), *passphrase_in_keychain),
            };
            WireSsh {
                host: h.host.clone(),
                port: Some(h.port),
                user: h.user.clone(),
                auth: auth.to_owned(),
                key_file,
                passphrase_in_keychain: passphrase,
            }
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use sqler_core::ConnectionConfig;

    use super::*;
    use crate::ConfigDir;

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn with_dir<T>(text: &str, f: impl FnOnce(&ConfigDir) -> T) -> T {
        let dir = std::env::temp_dir().join(format!(
            "sqler-config-{}-conn-{}-{:?}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("connections.toml"), text).unwrap();
        let r = f(&ConfigDir::new(&dir));
        let _ = std::fs::remove_dir_all(&dir);
        r
    }

    fn load(text: &str) -> Result<Vec<ConnectionConfig>, ConfigError> {
        with_dir(text, ConfigDir::load_connections)
    }

    fn load_store(text: &str) -> Result<ConnectionStore, ConfigError> {
        with_dir(text, ConfigDir::load_connection_store)
    }

    fn msg(text: &str) -> String {
        load(text).unwrap_err().to_string()
    }

    fn store_msg(text: &str) -> String {
        load_store(text).unwrap_err().to_string()
    }

    // ---- M1 tests, assertions unchanged ----

    #[test]
    fn valid_full_entry() {
        let v = load(
            "[[connection]]\nname = \"local\"\ndriver = \"sqlite\"\ntarget = \"app.db\"\ncolor = \"#22AA66\"\nenvironment = \"dev\"\n",
        )
        .unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].name, "local");
        assert_eq!(v[0].driver.as_str(), "sqlite");
        assert_eq!(v[0].target, "app.db");
        assert_eq!(v[0].color.as_deref(), Some("#22aa66"));
        assert_eq!(v[0].environment, Some(Environment::Dev));
    }

    #[test]
    fn optional_fields_absent_and_order_kept() {
        let v = load(
            "[[connection]]\nname = \"b\"\ndriver = \"sqlite\"\ntarget = \":memory:\"\n\n[[connection]]\nname = \"a\"\ndriver = \"sqlite\"\ntarget = \"x.db\"\n",
        )
        .unwrap();
        assert_eq!(v[0].name, "b");
        assert_eq!(v[1].name, "a");
        assert!(v[0].color.is_none() && v[0].environment.is_none());
    }

    #[test]
    fn empty_file_is_empty_list() {
        assert_eq!(load("").unwrap().len(), 0);
    }

    #[test]
    fn bad_values_rejected() {
        let base = |extra: &str| {
            format!("[[connection]]\nname = \"n\"\ndriver = \"sqlite\"\ntarget = \"t\"\n{extra}\n")
        };
        let e = msg(&base("color = \"red\""));
        assert!(
            e.contains("connection[0] \"n\"") && e.contains("#rrggbb"),
            "{e}"
        );
        assert!(msg(&base("color = \"#12345g\"")).contains("#rrggbb"));
        assert!(msg(&base("color = \"#12345\"")).contains("#rrggbb"));
        let e = msg(&base("environment = \"production\""));
        assert!(e.contains("dev, staging, prod"), "{e}");
        let e = msg("[[connection]]\nname = \"n\"\ndriver = \"SQLite\"\ntarget = \"t\"\n");
        assert!(e.contains("invalid driver id"), "{e}");
        let e = msg("[[connection]]\nname = \"  \"\ndriver = \"sqlite\"\ntarget = \"t\"\n");
        assert!(e.contains("name must not be empty"), "{e}");
    }

    #[test]
    fn duplicate_name_rejected() {
        let one = "[[connection]]\nname = \"n\"\ndriver = \"sqlite\"\ntarget = \"t\"\n";
        let e = msg(&format!("{one}{one}"));
        assert!(
            e.contains("connection[1] \"n\"") && e.contains("duplicate"),
            "{e}"
        );
    }

    #[test]
    fn secret_target_rejected_without_echo() {
        let e = msg(
            "[[connection]]\nname = \"p\"\ndriver = \"postgres\"\ntarget = \"postgres://bob:hunter2@db/x\"\n",
        );
        assert!(e.contains("contains a secret"), "{e}");
        assert!(e.contains("store the password in the keychain"), "{e}");
        assert!(
            !e.contains("hunter2") && !e.contains("bob") && !e.contains("postgres://"),
            "{e}"
        );
        let e = msg(
            "[[connection]]\nname = \"m\"\ndriver = \"mssql\"\ntarget = \"Server=h;Password=zzz9\"\n",
        );
        assert!(
            e.contains("contains a secret") && !e.contains("zzz9"),
            "{e}"
        );
    }

    #[test]
    fn unknown_fields_rejected() {
        let e = msg(
            "[[connection]]\nname = \"n\"\ndriver = \"sqlite\"\ntarget = \"t\"\npassword = \"x\"\n",
        );
        assert!(e.contains("unknown field"), "{e}");
        assert!(msg("[other]\nx = 1\n").contains("unknown field"));
    }

    #[test]
    fn missing_required_field_is_parse_error() {
        assert!(matches!(
            load("[[connection]]\nname = \"n\"\n"),
            Err(ConfigError::Parse { .. })
        ));
    }

    #[test]
    fn parse_error_does_not_echo_source_line() {
        let text = "[[connection]]\nname = \"n\"\ndriver = \"sqlite\"\ntarget = \"postgres://u:hunter2@h/d\" oops\n";
        let m = msg(text);
        assert!(!m.contains("hunter2"), "{m}");
        assert!(m.contains("line 4"), "{m}");
    }

    // ---- migration ----

    #[test]
    fn m1_file_migrates_with_ids_from_names_at_root() {
        let s = load_store(
            "[[connection]]\nname = \"My DB\"\ndriver = \"sqlite\"\ntarget = \"a.db\"\n\n[[connection]]\nname = \"my-db\"\ndriver = \"sqlite\"\ntarget = \"b.db\"\n",
        )
        .unwrap();
        let ids: Vec<_> = s.connections().iter().map(|c| c.id().as_str()).collect();
        assert_eq!(ids, ["my-db", "my-db-2"]);
        assert_eq!(s.groups().len(), 0);
        assert!(
            s.connections()
                .iter()
                .all(|c| c.group().is_none() && c.spec().tls.is_none() && c.spec().ssh.is_none())
        );
    }

    // ---- v1 ----

    const V1: &str = r##"version = 1

[[group]]
id = "clients"
name = "Clients"

[[group]]
id = "work"
name = "Work"
parent = "clients"

[[connection]]
id = "billing"
name = "Billing"
group = "work"
driver = "postgres"
target = "postgres://db/x?user=app"
color = "#22AA66"
environment = "prod"
password_in_keychain = true

[connection.tls]
mode = "verify-full"
ca_cert = "/etc/ca.pem"

[connection.ssh]
host = "bastion"
user = "me"
auth = "key-file"
key_file = "/home/me/.ssh/id"
passphrase_in_keychain = true

[[connection]]
id = "local"
name = "Local"
driver = "sqlite"
target = "app.db"
"##;

    #[test]
    fn v1_loads_all_fields() {
        let s = load_store(V1).unwrap();
        assert_eq!(s.groups().len(), 2);
        assert_eq!(s.groups()[1].parent().map(GroupId::as_str), Some("clients"));
        let b = &s.connections()[0];
        assert_eq!(b.id().as_str(), "billing");
        assert_eq!(b.group().map(GroupId::as_str), Some("work"));
        assert_eq!(b.spec().color.as_deref(), Some("#22aa66"));
        assert_eq!(b.spec().environment, Some(Environment::Prod));
        assert_eq!(b.spec().tls.as_ref().unwrap().mode, TlsMode::VerifyFull);
        let ssh = b.spec().ssh.as_ref().unwrap();
        assert_eq!((ssh.port, ssh.host.as_str()), (22, "bastion"));
        assert!(matches!(
            ssh.auth,
            SshAuth::KeyFile {
                passphrase_in_keychain: true,
                ..
            }
        ));
        assert_eq!(b.secret_refs().len(), 2);
        assert_eq!(s.connections()[1].group(), None);
        // `load_connections` flattens in file order.
        let flat = load(V1).unwrap();
        assert_eq!(flat[0].name, "Billing");
    }

    #[test]
    fn v1_serialises_and_reparses_equal() {
        let s = load_store(V1).unwrap();
        let text = serialize(Path::new("c.toml"), &s).unwrap();
        assert!(text.starts_with("version = 1"), "{text}");
        let again = parse(Path::new("c.toml"), &text).unwrap();
        assert_eq!(s, again);
    }

    #[test]
    fn v1_malformed_inputs() {
        let conn = |extra: &str| {
            format!(
                "version = 1\n[[group]]\nid = \"g\"\nname = \"G\"\n[[connection]]\nid = \"c\"\nname = \"C\"\ndriver = \"sqlite\"\ntarget = \"t\"\n{extra}\n"
            )
        };
        assert!(store_msg(&conn("password = \"x\"")).contains("unknown field"));
        assert!(store_msg(&conn("[connection.tls]\nmode = \"always\"")).contains("tls mode"));
        let e = store_msg(&conn(
            "[connection.ssh]\nhost = \"h\"\nuser = \"u\"\nauth = \"key-file\"",
        ));
        assert!(e.contains("requires key_file"), "{e}");
        let e = store_msg(&conn(
            "[connection.ssh]\nhost = \"h\"\nuser = \"u\"\nauth = \"magic\"",
        ));
        assert!(e.contains("password, key-file, agent"), "{e}");
        let e = store_msg(&conn(
            "[connection.ssh]\nhost = \"h\"\nuser = \"u\"\nauth = \"agent\"\npassphrase_in_keychain = true",
        ));
        assert!(e.contains("passphrase_in_keychain"), "{e}");
        let e = store_msg(&conn(
            "[connection.tls]\nmode = \"require\"\nclient_cert = \"c.pem\"",
        ));
        assert!(e.contains("client_cert and client_key"), "{e}");
        let e = store_msg(&conn("group = \"missing\""));
        assert!(
            e.contains("connection[0] \"C\"") && e.contains("does not exist"),
            "{e}"
        );
        let e = store_msg(&format!(
            "{}{}",
            conn(""),
            "[[connection]]\nid = \"c\"\nname = \"D\"\ndriver = \"sqlite\"\ntarget = \"t\"\n"
        ));
        assert!(e.contains("duplicate connection id"), "{e}");
        let e = store_msg(&format!(
            "{}{}",
            conn(""),
            "[[connection]]\nid = \"d\"\nname = \"C\"\ndriver = \"sqlite\"\ntarget = \"t\"\n"
        ));
        assert!(e.contains("duplicate connection name"), "{e}");
        let e = store_msg(&conn("").replace("id = \"c\"", "id = \"Bad Id\""));
        assert!(e.contains("lowercase"), "{e}");
    }

    #[test]
    fn v1_group_problems() {
        let g = |id: &str, name: &str, parent: &str| {
            let p = if parent.is_empty() {
                String::new()
            } else {
                format!("parent = \"{parent}\"\n")
            };
            format!("[[group]]\nid = \"{id}\"\nname = \"{name}\"\n{p}")
        };
        let e = store_msg(&format!("version = 1\n{}", g("a", "A", "nope")));
        assert!(e.contains("group[0] \"A\"") && e.contains("parent"), "{e}");
        let cyc = format!("version = 1\n{}{}", g("a", "A", "b"), g("b", "B", "a"));
        assert!(store_msg(&cyc).contains("cycle"));
        let selfp = format!("version = 1\n{}", g("a", "A", "a"));
        assert!(store_msg(&selfp).contains("cycle"));
        let dup = format!("version = 1\n{}{}", g("a", "A", ""), g("a", "B", ""));
        assert!(store_msg(&dup).contains("duplicate group id"));
        let dupn = format!("version = 1\n{}{}", g("a", "A", ""), g("b", "A", ""));
        assert!(store_msg(&dupn).contains("duplicate group name"));
        let ok = format!(
            "version = 1\n{}{}{}",
            g("a", "A", ""),
            g("b", "A", "a"),
            g("c", "B", "")
        );
        assert!(load_store(&ok).is_ok());
        let empty = format!("version = 1\n{}", g("a", " ", ""));
        assert!(store_msg(&empty).contains("name must not be empty"));
    }

    #[test]
    fn version_handling() {
        let e = store_msg("version = 2\n");
        assert!(
            e.contains("written by a newer SQLer (version 2, this build reads up to 1)"),
            "{e}"
        );
        assert!(store_msg("version = 0\n").contains("unsupported version 0"));
        let r = load_store("version = \"x\"\n");
        assert!(matches!(r, Err(ConfigError::Parse { .. })), "{r:?}");
        let r = load_store("version = -1\n");
        assert!(matches!(r, Err(ConfigError::Parse { .. })), "{r:?}");
        assert_eq!(load_store("version = 1\n").unwrap().connections().len(), 0);
    }

    #[test]
    fn v1_parse_error_does_not_echo_source_line() {
        let m = store_msg(
            "version = 1\n[[connection]]\nid = \"c\"\ntarget = \"postgres://u:hunter2@h/d\" oops\n",
        );
        assert!(!m.contains("hunter2") && m.contains("line 4"), "{m}");
    }
}
