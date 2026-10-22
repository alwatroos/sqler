// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The agents the picker offers (ADR-0051): the built-in profiles of `sqler-acp` plus the ones in
//! `<config>/agents.toml`.
//!
//! The file holds `[[agent]]` tables in the shape of [`AgentProfile`]; an entry with the id of a
//! built-in profile replaces it. It is read once, on the blocking pool, and never written. When it
//! is invalid the built-in profiles are offered and [`AgentProfiles::error`] says why: the text
//! names a line, never a value, because `env` may hold secrets.

use std::path::Path;

use serde::Deserialize;
use sqler_acp::{AgentProfile, builtin_profiles};

/// The name of the profiles file in the config directory.
pub const AGENTS_FILE: &str = "agents.toml";

/// The profiles to offer and the reason the file was not used, if it was not.
#[derive(Clone, PartialEq, Eq)]
pub struct AgentProfiles {
    /// The profiles, built-ins first (a replaced one keeps its place), then the user's.
    pub list: Vec<AgentProfile>,
    /// Why `agents.toml` was ignored.
    pub error: Option<String>,
}

// Never `env`.
impl std::fmt::Debug for AgentProfiles {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentProfiles")
            .field(
                "ids",
                &self.list.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
            )
            .field("error", &self.error)
            .finish()
    }
}

impl AgentProfiles {
    /// The built-in profiles only.
    #[must_use]
    pub fn builtin() -> Self {
        Self {
            list: builtin_profiles(),
            error: None,
        }
    }

    /// The profile with this id.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&AgentProfile> {
        self.list.iter().find(|p| p.id == id)
    }
}

#[derive(Deserialize)]
struct File {
    #[serde(default)]
    agent: Vec<AgentProfile>,
}

/// Reads `<dir>/agents.toml`. A missing file, or no directory, gives the built-in profiles. Does
/// blocking I/O: call it on the blocking pool.
#[must_use]
pub fn load_agent_profiles(dir: Option<&Path>) -> AgentProfiles {
    let mut profiles = AgentProfiles::builtin();
    let Some(dir) = dir else {
        return profiles;
    };
    let text = match std::fs::read_to_string(dir.join(AGENTS_FILE)) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return profiles,
        Err(_) => {
            profiles.error = Some(format!("{AGENTS_FILE} could not be read."));
            return profiles;
        }
    };
    match parse(&text) {
        Ok(user) => {
            for profile in user {
                match profiles.list.iter_mut().find(|p| p.id == profile.id) {
                    Some(slot) => *slot = profile,
                    None => profiles.list.push(profile),
                }
            }
        }
        Err(message) => profiles.error = Some(message),
    }
    profiles
}

/// The valid profiles of the file text, or a message for the user.
fn parse(text: &str) -> Result<Vec<AgentProfile>, String> {
    let file: File = toml::from_str(text).map_err(|err| {
        let at = err.span().map_or(String::new(), |span| {
            let line = text[..span.start.min(text.len())].matches('\n').count() + 1;
            format!(" near line {line}")
        });
        format!("{AGENTS_FILE} is not valid{at}. Only the built-in agents are offered.")
    })?;
    let mut seen = Vec::new();
    for profile in &file.agent {
        profile.validate().map_err(|err| {
            format!("{AGENTS_FILE}: {err}. Only the built-in agents are offered.")
        })?;
        if seen.contains(&profile.id.as_str()) {
            return Err(format!(
                "{AGENTS_FILE}: the id \"{}\" is used twice. Only the built-in agents are offered.",
                profile.id
            ));
        }
        seen.push(profile.id.as_str());
    }
    Ok(file.agent)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dir(std::path::PathBuf);

    impl Dir {
        fn new(tag: &str, file: Option<&str>) -> Self {
            let path = std::env::temp_dir()
                .join(format!("sqler-agent-profiles-{}-{tag}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            if let Some(text) = file {
                std::fs::write(path.join(AGENTS_FILE), text).unwrap();
            }
            Self(path)
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn ids(profiles: &AgentProfiles) -> Vec<&str> {
        profiles.list.iter().map(|p| p.id.as_str()).collect()
    }

    #[test]
    fn no_directory_and_no_file_give_the_built_ins() {
        let none = load_agent_profiles(None);
        assert_eq!(ids(&none), ["claude-code", "codex"]);
        assert_eq!(none.error, None);
        let dir = Dir::new("missing", None);
        let missing = load_agent_profiles(Some(&dir.0));
        assert_eq!(missing, none);
    }

    #[test]
    fn a_user_profile_is_added_and_a_built_in_id_is_replaced() {
        let dir = Dir::new(
            "ok",
            Some(
                r#"
[[agent]]
id = "gemini"
name = "Gemini"
command = "gemini"
args = ["--experimental-acp"]

[[agent]]
id = "codex"
name = "My Codex"
command = "/opt/codex-acp"
env = [["MODE", "fast"]]
"#,
            ),
        );
        let profiles = load_agent_profiles(Some(&dir.0));
        assert_eq!(profiles.error, None);
        assert_eq!(ids(&profiles), ["claude-code", "codex", "gemini"]);
        assert_eq!(profiles.get("codex").unwrap().name, "My Codex");
        assert_eq!(profiles.get("codex").unwrap().command, "/opt/codex-acp");
        assert_eq!(profiles.get("gemini").unwrap().args, ["--experimental-acp"]);
        assert!(profiles.get("nope").is_none());
    }

    #[test]
    fn an_invalid_file_gives_the_built_ins_and_a_reason_without_values() {
        let secret = "sk-canary-9981";
        for (tag, text) in [
            ("syntax", format!("[[agent]]\nid = \"a\"\nenv = [[\"K\", \"{secret}\"\n")),
            ("type", format!("[[agent]]\nid = 5\nname = \"{secret}\"\ncommand = \"x\"\n")),
            (
                "id",
                format!("[[agent]]\nid = \"Bad Id\"\nname = \"n\"\ncommand = \"x\"\nenv = [[\"K\", \"{secret}\"]]\n"),
            ),
            (
                "twice",
                "[[agent]]\nid = \"a\"\nname = \"n\"\ncommand = \"x\"\n[[agent]]\nid = \"a\"\nname = \"n\"\ncommand = \"y\"\n".to_owned(),
            ),
            ("empty-command", "[[agent]]\nid = \"a\"\nname = \"n\"\ncommand = \" \"\n".to_owned()),
        ] {
            let dir = Dir::new(tag, Some(&text));
            let profiles = load_agent_profiles(Some(&dir.0));
            assert_eq!(ids(&profiles), ["claude-code", "codex"], "{tag}");
            let error = profiles.error.clone().expect(tag);
            assert!(error.contains("agents.toml"), "{tag}: {error}");
            assert!(!error.contains(secret), "{tag}: {error}");
            assert!(!format!("{profiles:?}").contains(secret), "{tag}");
        }
    }

    #[test]
    fn a_file_that_cannot_be_read_is_reported() {
        let dir = Dir::new("unreadable", None);
        // A directory in place of the file.
        std::fs::create_dir(dir.0.join(AGENTS_FILE)).unwrap();
        let profiles = load_agent_profiles(Some(&dir.0));
        assert_eq!(ids(&profiles), ["claude-code", "codex"]);
        assert!(profiles.error.unwrap().contains("could not be read"));
    }
}
