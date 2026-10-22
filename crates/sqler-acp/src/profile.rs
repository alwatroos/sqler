// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Agent profiles: what to run, with which arguments, and which environment the agent gets.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::AcpError;

/// How to start one `ACP` agent. Built-in profiles come from [`builtin_profiles`]; a user can define more
/// (the type is serde friendly, storing them is the job of the settings layer).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentProfile {
    /// Short stable id: lowercase letters, digits and `-`.
    pub id: String,
    /// Name shown to the user.
    pub name: String,
    /// Program to run: a name looked up on `PATH`, or a path.
    pub command: String,
    /// Arguments, passed verbatim.
    #[serde(default)]
    pub args: Vec<String>,
    /// Literal environment variables for the agent (applied last).
    #[serde(default)]
    pub env: Vec<(String, String)>,
    /// Names of variables copied from `SQLer`'s own environment when set (for example the user's own login).
    #[serde(default)]
    pub inherit_env: Vec<String>,
    /// Shown when the program is not found.
    #[serde(default)]
    pub install_hint: Option<String>,
}

/// Variables every agent gets from the parent environment.
const BASE_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "LANG",
    "TERM",
    "TMPDIR",
    "TEMP",
    "TMP",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    "SSL_CERT_FILE",
    "NODE_EXTRA_CA_CERTS",
];

/// Extra base variables on Windows.
const BASE_ENV_WINDOWS: &[&str] = &[
    "SystemRoot",
    "windir",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "ProgramData",
    "ProgramFiles",
    "ComSpec",
    "PATHEXT",
];

/// Prefixes of base variables (`LC_*`, `XDG_*`).
const BASE_ENV_PREFIXES: &[&str] = &["LC_", "XDG_"];

/// The profiles that ship with `SQLer`: `claude-code` and `codex`.
#[must_use]
pub fn builtin_profiles() -> Vec<AgentProfile> {
    vec![
        AgentProfile {
            id: "claude-code".to_owned(),
            name: "Claude Code".to_owned(),
            command: "claude-agent-acp".to_owned(),
            args: Vec::new(),
            env: Vec::new(),
            inherit_env: vec![
                "ANTHROPIC_API_KEY".to_owned(),
                "CLAUDE_CONFIG_DIR".to_owned(),
            ],
            install_hint: Some("npm install -g @agentclientprotocol/claude-agent-acp".to_owned()),
        },
        AgentProfile {
            id: "codex".to_owned(),
            name: "Codex".to_owned(),
            command: "codex-acp".to_owned(),
            args: Vec::new(),
            env: Vec::new(),
            inherit_env: vec!["OPENAI_API_KEY".to_owned(), "CODEX_HOME".to_owned()],
            install_hint: Some("npm install -g @agentclientprotocol/codex-acp".to_owned()),
        },
    ]
}

impl AgentProfile {
    /// Check the profile: the id is `[a-z0-9-]+` and the command is not empty.
    ///
    /// # Errors
    /// [`AcpError::InvalidProfile`] naming what is wrong.
    pub fn validate(&self) -> Result<(), AcpError> {
        let id_ok = !self.id.is_empty()
            && self
                .id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        if !id_ok {
            return Err(AcpError::InvalidProfile(
                "the id must be non-empty and use only a-z, 0-9 and '-'".to_owned(),
            ));
        }
        if self.command.trim().is_empty() {
            return Err(AcpError::InvalidProfile(format!(
                "profile \"{}\" has an empty command",
                self.id
            )));
        }
        Ok(())
    }
}

/// Whether two environment names are the same: case-insensitive on Windows, exact elsewhere.
fn same_name(a: &str, b: &str) -> bool {
    if cfg!(windows) {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

fn has_prefix(name: &str, prefix: &str) -> bool {
    if cfg!(windows) {
        name.len() >= prefix.len()
            && name.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
    } else {
        name.starts_with(prefix)
    }
}

fn is_base_name(name: &str) -> bool {
    BASE_ENV.iter().any(|n| same_name(n, name))
        || (cfg!(windows)
            && BASE_ENV_WINDOWS
                .iter()
                .any(|n| n.eq_ignore_ascii_case(name)))
        || BASE_ENV_PREFIXES.iter().any(|p| has_prefix(name, p))
}

fn set(out: &mut Vec<(OsString, OsString)>, key: OsString, value: OsString) {
    let same = |k: &OsString| match (k.to_str(), key.to_str()) {
        (Some(a), Some(b)) => same_name(a, b),
        _ => *k == key,
    };
    if let Some(slot) = out.iter_mut().find(|(k, _)| same(k)) {
        slot.1 = value;
    } else {
        out.push((key, value));
    }
}

/// The complete environment of an agent process (the child starts from an empty one).
///
/// Order: the fixed base allowlist copied from `parent`, then the names in `inherit_env`, then the profile's
/// literal `env` (which wins). Nothing else from `parent` is passed, so no `SQLer` secret reaches the agent.
#[must_use]
pub fn agent_env(
    profile: &AgentProfile,
    parent: impl IntoIterator<Item = (OsString, OsString)>,
) -> Vec<(OsString, OsString)> {
    let parent: Vec<(OsString, OsString)> = parent.into_iter().collect();
    let mut out: Vec<(OsString, OsString)> = Vec::new();
    for (key, value) in &parent {
        if key.to_str().is_some_and(is_base_name) {
            set(&mut out, key.clone(), value.clone());
        }
    }
    for wanted in &profile.inherit_env {
        if let Some((key, value)) = parent
            .iter()
            .find(|(k, _)| k.to_str().is_some_and(|k| same_name(k, wanted)))
        {
            set(&mut out, key.clone(), value.clone());
        }
    }
    for (key, value) in &profile.env {
        set(&mut out, OsString::from(key), OsString::from(value));
    }
    out
}

fn is_runnable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Find the program to run.
///
/// A command with a path separator (absolute or relative) is used as is when it exists. A bare name is
/// searched in `path` (the current directory is not searched). On Windows a name without an extension is
/// tried with the `PATHEXT` extensions first and never bare, because npm installs an extensionless shell
/// script next to its `.cmd` shim and only the shim can be spawned; a name that already has an extension is
/// tried as it is.
#[must_use]
pub fn resolve_program(command: &str, path: Option<&OsStr>) -> Option<PathBuf> {
    let pathext = cfg!(windows)
        .then(|| std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_owned()));
    resolve_with(command, path, pathext.as_deref())
}

/// `resolve_program` with the Windows behaviour chosen by `pathext` (`Some` is the `PATHEXT` value).
fn resolve_with(command: &str, path: Option<&OsStr>, pathext: Option<&str>) -> Option<PathBuf> {
    let direct = Path::new(command);
    if command.contains('/') || command.contains(std::path::MAIN_SEPARATOR) {
        return is_runnable(direct).then(|| direct.to_path_buf());
    }
    let names: Vec<String> = match pathext {
        Some(raw) if direct.extension().is_none() => raw
            .split(';')
            .filter(|e| !e.is_empty())
            .map(|ext| format!("{command}{ext}"))
            .collect(),
        _ => vec![command.to_owned()],
    };
    for dir in std::env::split_paths(path?) {
        for name in &names {
            let candidate = dir.join(name);
            if is_runnable(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile() -> AgentProfile {
        AgentProfile {
            id: "demo".to_owned(),
            name: "Demo".to_owned(),
            command: "demo-acp".to_owned(),
            args: vec![],
            env: vec![],
            inherit_env: vec![],
            install_hint: None,
        }
    }

    fn os(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        pairs
            .iter()
            .map(|(k, v)| (OsString::from(k), OsString::from(v)))
            .collect()
    }

    #[cfg(windows)]
    #[test]
    fn windows_names_are_case_insensitive() {
        let mut p = profile();
        p.inherit_env = vec!["ANTHROPIC_API_KEY".to_owned()];
        let env = agent_env(
            &p,
            os(&[
                ("Path", "C:\\bin"),
                ("anthropic_api_key", "sk"),
                ("SQLER_SECRET", "x"),
            ]),
        );
        let keys: Vec<&str> = env.iter().filter_map(|(k, _)| k.to_str()).collect();
        assert!(keys.contains(&"Path"));
        assert!(keys.contains(&"anthropic_api_key"));
        assert!(!keys.contains(&"SQLER_SECRET"));
    }

    #[test]
    fn validate_accepts_and_refuses() {
        assert!(profile().validate().is_ok());
        for bad in ["", "Upper", "has space", "under_score"] {
            let mut p = profile();
            p.id = bad.to_owned();
            assert!(
                matches!(p.validate(), Err(AcpError::InvalidProfile(_))),
                "{bad}"
            );
        }
        let mut p = profile();
        p.command = "  ".to_owned();
        assert!(matches!(p.validate(), Err(AcpError::InvalidProfile(_))));
    }

    #[test]
    fn builtins_are_valid() {
        let all = builtin_profiles();
        let ids: Vec<&str> = all.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["claude-code", "codex"]);
        for p in &all {
            assert!(p.validate().is_ok());
            assert!(
                p.install_hint
                    .as_deref()
                    .is_some_and(|h| h.contains("npm install -g"))
            );
        }
        assert!(all[0].inherit_env.contains(&"ANTHROPIC_API_KEY".to_owned()));
        assert!(all[1].inherit_env.contains(&"OPENAI_API_KEY".to_owned()));
    }

    #[test]
    fn env_drops_secrets_and_keeps_the_allowlist() {
        let parent = os(&[
            ("PATH", "/bin"),
            ("HOME", "/home/u"),
            ("LC_ALL", "C"),
            ("XDG_CONFIG_HOME", "/c"),
            ("https_proxy", "http://p"),
            ("SQLER_SECRET", "s3cret"),
            ("AWS_SECRET_ACCESS_KEY", "aws"),
            ("CARGO_HOME", "/cargo"),
            ("CARGO_PKG_NAME", "x"),
            ("ANTHROPIC_API_KEY", "sk-user"),
        ]);
        let env = agent_env(&profile(), parent);
        let keys: Vec<&str> = env.iter().filter_map(|(k, _)| k.to_str()).collect();
        for kept in ["PATH", "HOME", "LC_ALL", "XDG_CONFIG_HOME", "https_proxy"] {
            assert!(keys.contains(&kept), "{kept}");
        }
        for dropped in [
            "SQLER_SECRET",
            "AWS_SECRET_ACCESS_KEY",
            "CARGO_HOME",
            "CARGO_PKG_NAME",
            "ANTHROPIC_API_KEY",
        ] {
            assert!(!keys.contains(&dropped), "{dropped}");
        }
    }

    #[test]
    fn env_inherit_and_literals_apply_in_order() {
        let mut p = profile();
        p.inherit_env = vec!["ANTHROPIC_API_KEY".to_owned(), "NOT_SET".to_owned()];
        p.env = vec![
            ("HOME".to_owned(), "/override".to_owned()),
            ("EXTRA".to_owned(), "1".to_owned()),
        ];
        let env = agent_env(
            &p,
            os(&[("HOME", "/home/u"), ("ANTHROPIC_API_KEY", "sk-user")]),
        );
        let get = |k: &str| {
            env.iter()
                .find(|(n, _)| n == OsStr::new(k))
                .map(|(_, v)| v.clone())
        };
        assert_eq!(get("ANTHROPIC_API_KEY"), Some(OsString::from("sk-user")));
        assert_eq!(get("HOME"), Some(OsString::from("/override")));
        assert_eq!(get("EXTRA"), Some(OsString::from("1")));
        assert_eq!(get("NOT_SET"), None);
        assert_eq!(
            env.iter().filter(|(k, _)| k == OsStr::new("HOME")).count(),
            1
        );
    }

    fn make_exe(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, "x").expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        path
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sqler-acp-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    #[test]
    fn resolve_searches_path_and_accepts_paths() {
        let dir = temp_dir("resolve");
        #[cfg(windows)]
        let name = "demo-acp.cmd";
        #[cfg(not(windows))]
        let name = "demo-acp";
        let exe = make_exe(&dir, name);
        let path = std::env::join_paths([dir.clone()]).expect("join");
        // PATHEXT supplies the extension in its own case on Windows.
        let found = resolve_program("demo-acp", Some(&path)).expect("found on PATH");
        assert_eq!(
            found.to_string_lossy().to_lowercase(),
            dir.join(name).to_string_lossy().to_lowercase()
        );
        assert_eq!(resolve_program("missing-acp", Some(&path)), None);
        assert_eq!(resolve_program("demo-acp", None), None);
        assert_eq!(
            resolve_program(exe.to_str().expect("utf8"), None),
            Some(exe.clone())
        );
        assert_eq!(
            resolve_program(dir.join("nope").to_str().expect("utf8"), None),
            None
        );
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn windows_resolution_prefers_the_shim_over_the_extensionless_script() {
        let dir = temp_dir("shim");
        make_exe(&dir, "x");
        make_exe(&dir, "x.CMD");
        make_exe(&dir, "y.CMD");
        let path = std::env::join_paths([dir.clone()]).expect("join");
        let lower = |p: Option<PathBuf>| p.map(|p| p.to_string_lossy().to_lowercase());
        let want = |name: &str| Some(dir.join(name).to_string_lossy().to_lowercase());
        let exts = Some(".COM;.EXE;.BAT;.CMD");
        // Without an extension the bare script is never picked, whatever the case of the shim.
        assert_eq!(lower(resolve_with("x", Some(&path), exts)), want("x.cmd"));
        assert_eq!(lower(resolve_with("y", Some(&path), exts)), want("y.cmd"));
        // A command with an extension is tried as it is.
        assert_eq!(
            lower(resolve_with("x.CMD", Some(&path), exts)),
            want("x.cmd")
        );
        // A bare script alone is not a Windows program.
        make_exe(&dir, "z");
        assert_eq!(resolve_with("z", Some(&path), exts), None);
        // Other platforms keep the bare name.
        assert_eq!(resolve_with("x", Some(&path), None), Some(dir.join("x")));
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn resolve_skips_non_executable_files() {
        let dir = temp_dir("noexec");
        std::fs::write(dir.join("plain"), "x").expect("write");
        let path = std::env::join_paths([dir.clone()]).expect("join");
        assert_eq!(resolve_program("plain", Some(&path)), None);
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn profile_roundtrips_through_toml() {
        let text = r#"
            id = "mine"
            name = "Mine"
            command = "my-agent"
            args = ["--acp"]
            env = [["KEY", "value"]]
        "#;
        let p: AgentProfile = toml::from_str(text).expect("parse");
        assert_eq!(p.args, ["--acp"]);
        assert_eq!(p.env, [("KEY".to_owned(), "value".to_owned())]);
        assert!(p.inherit_env.is_empty() && p.install_hint.is_none());
    }
}
