// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

use std::collections::HashSet;
use std::path::Path;

use serde::Deserialize;
use sqler_core::{ConnectionConfig, DriverId, Environment};

use crate::fs::read_optional;
use crate::{ConfigDir, ConfigError};

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ConnectionsFile {
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

fn is_hex_color(s: &str) -> bool {
    s.len() == 7 && s.starts_with('#') && s[1..].chars().all(|c| c.is_ascii_hexdigit())
}

fn convert(path: &Path, i: usize, e: &Entry) -> Result<ConnectionConfig, ConfigError> {
    let fail = |msg: String| ConfigError::Invalid {
        path: path.to_owned(),
        message: format!("connection[{i}] \"{}\": {msg}", e.name),
    };
    if e.name.trim().is_empty() {
        return Err(fail("name must not be empty".to_owned()));
    }
    let driver = DriverId::new(&e.driver).map_err(|err| fail(err.to_string()))?;
    let mut cfg = ConnectionConfig::new(e.name.clone(), driver, e.target.clone());
    if let Some(color) = &e.color {
        if !is_hex_color(color) {
            return Err(fail(format!("color {color:?} must look like #rrggbb")));
        }
        cfg = cfg.with_color(color.to_ascii_lowercase());
    }
    if let Some(env) = &e.environment {
        let env = Environment::from_id(env).ok_or_else(|| {
            fail(format!(
                "environment {env:?} is not one of: dev, staging, prod"
            ))
        })?;
        cfg = cfg.with_environment(env);
    }
    if cfg.target_has_secret() {
        return Err(fail(
            "contains a secret; secrets are not stored in config files (keychain arrives in M2)"
                .to_owned(),
        ));
    }
    Ok(cfg)
}

impl ConfigDir {
    /// Loads the saved connections from `connections.toml` in file order;
    /// a missing file yields an empty list. The file is read-only in M1.
    ///
    /// # Errors
    /// [`ConfigError::Read`], [`ConfigError::Parse`], or
    /// [`ConfigError::Invalid`] naming the connection (index and name) whose
    /// field is bad. A target that embeds a secret is rejected without being echoed.
    pub fn load_connections(&self) -> Result<Vec<ConnectionConfig>, ConfigError> {
        let path = self.connections_path();
        let Some(text) = read_optional(&path)? else {
            return Ok(Vec::new());
        };
        let file: ConnectionsFile = toml::from_str(&text).map_err(|e| ConfigError::Parse {
            path: path.clone(),
            message: parse_message(&text, &e),
        })?;
        let mut seen = HashSet::new();
        let mut out = Vec::with_capacity(file.connections.len());
        for (i, entry) in file.connections.into_iter().enumerate() {
            let cfg = convert(&path, i, &entry)?;
            if !seen.insert(cfg.name.clone()) {
                return Err(ConfigError::Invalid {
                    path: path.clone(),
                    message: format!(
                        "connection[{i}] \"{}\": duplicate connection name",
                        cfg.name
                    ),
                });
            }
            out.push(cfg);
        }
        Ok(out)
    }
}

/// Describes a parse error by line, column and message only. The `toml` Display
/// output quotes the offending source line, which may hold a password.
fn parse_message(text: &str, e: &toml::de::Error) -> String {
    match e.span() {
        Some(span) => {
            let before = &text[..span.start.min(text.len())];
            let line = before.matches('\n').count() + 1;
            let col = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
            format!("line {line}, column {col}: {}", e.message())
        }
        None => e.message().to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn load(text: &str) -> Result<Vec<ConnectionConfig>, ConfigError> {
        let dir = std::env::temp_dir().join(format!(
            "sqler-config-{}-conn-{}-{:?}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("connections.toml"), text).unwrap();
        let r = ConfigDir::new(&dir).load_connections();
        let _ = std::fs::remove_dir_all(&dir);
        r
    }

    fn msg(text: &str) -> String {
        load(text).unwrap_err().to_string()
    }

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
}
