// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! What the permission dialog shows (ADR-0059): [`GrantPrompt`] and one [`CapabilityLine`] per
//! requested capability. Plain data, no GPUI; the overlay in `view.rs` only draws it.
//!
//! Text that comes from the manifest is untrusted: control and bidirectional marks are replaced
//! before it is shown. Capability arguments are shown verbatim, in monospace, because the user
//! has to be able to compare them with what they expect (punycode hosts stay punycode).

use sqler_config::{KeyChord, Modifiers};
use sqler_plugin_api::{
    Capability, CapabilityKind, CapabilitySet, FsRoot, GrantKey, HostPattern, Manifest,
};

use super::GrantCommand;
use super::book::GrantStatus;

/// The longest plugin name shown, in characters.
const NAME_MAX_CHARS: usize = 80;
/// The longest description shown, in bytes.
const DESCRIPTION_MAX_BYTES: usize = 1024;

/// One line of the dialog: a requested capability in words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityLine {
    /// The kind, which selects the icon.
    pub kind: CapabilityKind,
    /// The capability in the grammar of the manifest, for example `db.read`.
    pub capability: String,
    /// What it allows, in a sentence.
    pub summary: &'static str,
    /// The argument verbatim (host pattern, scope, engine), shown in monospace.
    pub detail: Option<String>,
    /// Extra remarks the user should read.
    pub notes: Vec<String>,
    /// Whether the line is drawn with a warning.
    pub prominent: bool,
    /// Whether an earlier decision did not include it. Only set on a re-prompt.
    pub is_new: bool,
}

/// The model of the permission dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantPrompt {
    /// The grant key in text form; Approve stores the decision under it.
    pub key: String,
    /// The plugin id.
    pub plugin_id: String,
    /// The plugin version.
    pub version: String,
    /// The display name, sanitised.
    pub name: String,
    /// The description, sanitised.
    pub description: Option<String>,
    /// `Allow plugin <name>?`.
    pub title: String,
    /// What Approve grants.
    pub capabilities: CapabilitySet,
    /// The capabilities in words: prominent lines first, then set order.
    pub lines: Vec<CapabilityLine>,
    /// What the plugin adds, for example `3 commands`; zero counts are left out.
    pub contributions: Vec<String>,
    /// Whether the plugin had an earlier decision under another key.
    pub reprompt: bool,
    /// Why Approve is not possible, if it is not.
    pub problem: Option<String>,
}

impl GrantPrompt {
    #[allow(clippy::needless_pass_by_value)] // the signature is the contract with the plugin manager
    /// Builds the prompt for `manifest` under `key`, given what the book says about it.
    #[must_use]
    pub fn new(manifest: &Manifest, key: GrantKey, status: &GrantStatus) -> Self {
        let (previous, problem) = match status {
            GrantStatus::NeedsPrompt { previous } => (previous.as_ref(), None),
            GrantStatus::Unavailable { problem } => (None, Some(sanitize(problem))),
            _ => (None, None),
        };
        let name = clip_chars(&sanitize(&manifest.name), NAME_MAX_CHARS);
        let mut lines: Vec<CapabilityLine> = manifest
            .capabilities
            .iter()
            .map(|c| line(c, previous))
            .collect();
        // Stable: prominent rows first, set order inside each group.
        lines.sort_by_key(|l| !l.prominent);
        Self {
            key: sanitize(&key.to_string()),
            plugin_id: sanitize(key.id().as_str()),
            version: sanitize(&key.version().to_string()),
            title: format!("Allow plugin {name}?"),
            name,
            description: manifest
                .description
                .as_deref()
                .map(|d| clip_bytes(&sanitize(d), DESCRIPTION_MAX_BYTES)),
            capabilities: manifest.capabilities.clone(),
            lines,
            contributions: contributions(manifest),
            reprompt: previous.is_some(),
            problem,
        }
    }

    /// Whether Approve can be offered.
    #[must_use]
    pub fn can_approve(&self) -> bool {
        self.problem.is_none()
    }

    /// How many lines are drawn with a warning.
    #[must_use]
    pub fn attention_count(&self) -> usize {
        self.lines.iter().filter(|l| l.prominent).count()
    }

    /// The always-visible summary, for example `5 permissions, 3 need attention`.
    #[must_use]
    pub fn summary(&self) -> String {
        let n = self.lines.len();
        let m = self.attention_count();
        let permissions = if n == 1 {
            "1 permission".to_owned()
        } else {
            format!("{n} permissions")
        };
        let attention = match m {
            0 => "none need attention".to_owned(),
            1 => "1 needs attention".to_owned(),
            _ => format!("{m} need attention"),
        };
        format!("{permissions}, {attention}")
    }

    /// The command a key press runs in the dialog: plain Escape is Deny, nothing else is bound.
    /// No key chord maps to Approve.
    #[must_use]
    pub fn key_command(chord: &KeyChord) -> Option<GrantCommand> {
        (chord.modifiers() == Modifiers::default() && chord.key() == "escape")
            .then_some(GrantCommand::Deny)
    }
}

/// Replaces control characters and bidirectional marks with U+FFFD.
fn sanitize(text: &str) -> String {
    text.chars()
        .map(|c| {
            let bidi = matches!(c, '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}');
            if c.is_control() || bidi { '\u{FFFD}' } else { c }
        })
        .collect()
}

fn clip_chars(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

fn clip_bytes(text: &str, max: usize) -> String {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

fn count(n: usize, one: &str, many: &str) -> Option<String> {
    match n {
        0 => None,
        1 => Some(format!("1 {one}")),
        n => Some(format!("{n} {many}")),
    }
}

fn contributions(m: &Manifest) -> Vec<String> {
    let c = &m.contributes;
    [
        count(c.commands.len(), "command", "commands"),
        count(c.panels.len(), "panel", "panels"),
        count(c.status_items.len(), "status item", "status items"),
        count(c.menus.len(), "menu entry", "menu entries"),
        count(c.exporters.len(), "exporter", "exporters"),
        count(
            usize::from(c.completion.is_some()),
            "completion provider",
            "completion providers",
        ),
        count(c.ai_tools.len(), "AI tool", "AI tools"),
        count(c.drivers.len(), "driver", "drivers"),
        count(c.settings.len(), "setting", "settings"),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn plain(kind: CapabilityKind, capability: &Capability, summary: &'static str) -> CapabilityLine {
    CapabilityLine {
        kind,
        capability: capability.to_string(),
        summary,
        detail: None,
        notes: Vec::new(),
        prominent: false,
        is_new: false,
    }
}

fn has_punycode(host: &str) -> bool {
    host.split('.')
        .any(|label| label.to_ascii_lowercase().starts_with("xn--"))
}

fn http_line(capability: &Capability, pattern: &HostPattern) -> CapabilityLine {
    let mut l = plain(CapabilityKind::NetHttp, capability, "Connect to a web host");
    l.detail = Some(pattern.to_string());
    l.notes.push(match pattern.port() {
        Some(p) => format!("port {p}"),
        None => "ports 80 and 443".to_owned(),
    });
    if pattern.is_wildcard() {
        l.notes
            .push(format!("Includes every subdomain of {}.", pattern.host()));
    }
    if has_punycode(pattern.host()) {
        l.prominent = true;
        l.notes.push(
            "Internationalised name shown in its encoded (punycode) form; check it carefully."
                .to_owned(),
        );
    }
    if is_localhost(pattern.host()) {
        l.prominent = true;
        l.notes.push("This is your own computer.".to_owned());
    }
    l
}

/// `localhost` and every `*.localhost` name (RFC 6761: loopback).
fn is_localhost(host: &str) -> bool {
    host.rsplit('.')
        .next()
        .is_some_and(|last| last.eq_ignore_ascii_case("localhost"))
}

fn line(capability: &Capability, previous: Option<&CapabilitySet>) -> CapabilityLine {
    let mut l = match capability {
        Capability::DbRead => plain(
            CapabilityKind::DbRead,
            capability,
            "Read the catalog and run read-only queries on your connections",
        ),
        Capability::DbWrite => {
            let mut l = plain(
                CapabilityKind::DbWrite,
                capability,
                "Ask to change data or schema on your connections",
            );
            l.notes.push(
                "Every change goes through the same confirmation as your own statements."
                    .to_owned(),
            );
            l.prominent = true;
            l
        }
        Capability::UiPanel => plain(CapabilityKind::UiPanel, capability, "Show its own panels"),
        Capability::AiTool => plain(
            CapabilityKind::AiTool,
            capability,
            "Offer tools to the AI assistant",
        ),
        Capability::DriverGuest => plain(
            CapabilityKind::DriverGuest,
            capability,
            "Provide a database driver that runs inside the plugin",
        ),
        Capability::DriverHost(engine) => {
            let mut l = plain(
                CapabilityKind::DriverHost,
                capability,
                "Use a built-in database engine",
            );
            l.detail = Some(engine.to_string());
            l
        }
        Capability::NetHttp(pattern) => http_line(capability, pattern),
        Capability::FsRead(scope) => match scope.root() {
            FsRoot::Plugin => fs_line(capability, "Read files in its own install folder", scope),
            FsRoot::Data => fs_line(capability, "Read files in its own data folder", scope),
            FsRoot::Home => {
                let mut l = fs_line(capability, "Read files in your home folder", scope);
                l.prominent = true;
                let other_apps = scope.segments().first().is_some_and(|s| {
                    s.eq_ignore_ascii_case("Library") || s.eq_ignore_ascii_case("AppData")
                });
                if other_apps {
                    l.notes.push(
                        "This folder holds the settings and data of other applications.".to_owned(),
                    );
                }
                l
            }
            _ => unknown(capability),
        },
        _ => unknown(capability),
    };
    l.is_new = previous.is_some_and(|p| !p.contains(capability));
    l
}

fn fs_line(
    capability: &Capability,
    summary: &'static str,
    scope: &sqler_plugin_api::FsScope,
) -> CapabilityLine {
    let mut l = plain(CapabilityKind::FsRead, capability, summary);
    l.detail = Some(scope.to_string());
    l
}

fn unknown(capability: &Capability) -> CapabilityLine {
    let mut l = plain(capability.kind(), capability, "Unknown permission");
    l.detail = Some(capability.to_string());
    l.prominent = true;
    l
}

/// A manifest for tests: `caps` are capability strings; `entry` adds a component and a command.
#[cfg(any(test, feature = "screenshots"))]
#[allow(clippy::expect_used)] // fixed fixture text
pub(crate) fn fixture_manifest(id: &str, version: &str, caps: &[&str], entry: bool) -> Manifest {
    let caps = caps
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let mut text = format!(
        "[plugin]\nid = \"{id}\"\nname = \"Acme Sync\"\nversion = \"{version}\"\napi = \"0.1\"\n\
         description = \"Keeps a reporting database in step with your warehouse.\"\n\
         capabilities = [{caps}]\n"
    );
    if entry {
        text.push_str(
            "[entry]\ncomponent = \"bin/plugin.wasm\"\n\
             [[commands]]\nid = \"sync\"\ntitle = \"Sync now\"\n\
             [[commands]]\nid = \"status\"\ntitle = \"Show status\"\n\
             [[commands]]\nid = \"reset\"\ntitle = \"Reset\"\n",
        );
    }
    Manifest::parse(&text).expect("fixture manifest parses")
}

/// The key of a fixture manifest (with a component digest when it has an `[entry]`).
#[cfg(any(test, feature = "screenshots"))]
#[allow(clippy::expect_used)] // fixed fixture text
pub(crate) fn fixture_key(m: &Manifest) -> GrantKey {
    let digest = sqler_plugin_api::ComponentDigest::of(b"wasm");
    m.grant_key(m.entry.is_some().then_some(&digest))
        .expect("fixture key")
}

/// The prompt of the `plugins-grant` scene: a re-prompt with a new, prominent set.
#[cfg(any(test, feature = "screenshots"))]
#[allow(clippy::expect_used)] // fixed fixture text
pub(crate) fn sample_prompt() -> GrantPrompt {
    let m = fixture_manifest(
        "acme-sync",
        "1.3.0",
        &[
            "db.read",
            "db.write",
            "net.http:*.xn--bcher-kva.example",
            "fs.read:home/Library/Preferences",
            "fs.read:data",
        ],
        true,
    );
    let mut previous = CapabilitySet::new();
    previous.insert("db.read".parse().expect("capability"));
    GrantPrompt::new(
        &m,
        fixture_key(&m),
        &GrantStatus::NeedsPrompt {
            previous: Some(previous),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqler_config::Platform;

    fn prompt_for(caps: &[&str]) -> GrantPrompt {
        let m = fixture_manifest("acme-sync", "1.0.0", caps, true);
        GrantPrompt::new(
            &m,
            fixture_key(&m),
            &GrantStatus::NeedsPrompt { previous: None },
        )
    }

    fn only(caps: &str) -> CapabilityLine {
        let all: &[&str] = if caps == "db.write" {
            &["db.read", "db.write"]
        } else {
            &[caps]
        };
        let p = prompt_for(all);
        p.lines.into_iter().find(|l| l.capability == caps).unwrap()
    }

    #[test]
    fn every_kind_has_text() {
        for c in [
            "db.read",
            "db.write",
            "ui.panel",
            "ai.tool",
            "driver.guest",
            "driver.host:duckdb",
            "net.http:example.com",
            "fs.read:plugin",
            "fs.read:data/cache",
            "fs.read:home/Documents",
        ] {
            let l = only(c);
            assert!(!l.summary.is_empty(), "{c}");
            assert_ne!(l.summary, "Unknown permission", "{c}");
            assert_eq!(l.capability, c);
        }
        assert_eq!(only("driver.host:duckdb").detail.as_deref(), Some("duckdb"));
        assert!(only("db.write").prominent);
        assert!(!only("db.read").prominent);
    }

    #[test]
    fn punycode_is_shown_as_is_with_a_note() {
        let l = only("net.http:*.xn--bcher-kva.example");
        assert_eq!(l.detail.as_deref(), Some("*.xn--bcher-kva.example"));
        assert!(l.prominent);
        assert!(l.notes.iter().any(|n| n.contains("punycode")));
        assert!(l.notes.iter().any(|n| n.contains("every subdomain")));
        assert!(l.notes.iter().any(|n| n == "ports 80 and 443"));
        assert!(!only("net.http:example.com").prominent);
        assert!(
            only("net.http:example.com:8080")
                .notes
                .contains(&"port 8080".to_owned())
        );
    }

    #[test]
    fn localhost_is_flagged() {
        let l = only("net.http:localhost:9000");
        assert!(l.prominent);
        assert!(l.notes.iter().any(|n| n.contains("your own computer")));
    }

    #[test]
    fn any_localhost_name_is_flagged() {
        for host in ["foo.localhost", "a.b.localhost"] {
            let l = only(&format!("net.http:{host}"));
            assert!(l.prominent, "{host}");
            assert!(l.notes.iter().any(|n| n.contains("your own computer")));
        }
        assert!(!only("net.http:localhost.example.com").prominent);
        assert!(!only("net.http:notlocalhost.com").prominent);
    }

    #[test]
    fn the_summary_counts_permissions_and_warnings() {
        let p = prompt_for(&["db.read", "fs.read:data", "fs.read:home/Library/x"]);
        assert_eq!(p.summary(), "3 permissions, 1 needs attention");
        assert_eq!(
            prompt_for(&["db.read"]).summary(),
            "1 permission, none need attention"
        );
        let p = prompt_for(&["fs.read:home/Library/x", "fs.read:home/AppData/y"]);
        assert_eq!(p.summary(), "2 permissions, 2 need attention");
    }

    #[test]
    fn home_scopes_are_prominent_and_other_apps_are_warned() {
        for scope in [
            "fs.read:home/Library/Preferences",
            "fs.read:home/library",
            "fs.read:home/AppData/Roaming",
            "fs.read:home/APPDATA",
        ] {
            let l = only(scope);
            assert!(l.prominent, "{scope}");
            assert!(
                l.notes.iter().any(|n| n.contains("other applications")),
                "{scope}"
            );
        }
        let docs = only("fs.read:home/Documents");
        assert!(docs.prominent);
        assert_eq!(docs.notes.len(), 0);
        assert!(!only("fs.read:data").prominent);
        assert!(!only("fs.read:plugin").prominent);
        assert_eq!(docs.detail.as_deref(), Some("home/Documents"));
    }

    #[test]
    fn prominent_rows_come_first_and_new_ones_are_marked() {
        let m = fixture_manifest(
            "acme-sync",
            "2.0.0",
            &["db.read", "ui.panel", "db.write", "fs.read:home/Documents"],
            true,
        );
        let mut previous = CapabilitySet::new();
        previous.insert(Capability::DbRead);
        let p = GrantPrompt::new(
            &m,
            fixture_key(&m),
            &GrantStatus::NeedsPrompt {
                previous: Some(previous),
            },
        );
        assert!(p.reprompt);
        let prominent = p.lines.iter().take_while(|l| l.prominent).count();
        assert_eq!(prominent, 2);
        assert!(p.lines[prominent..].iter().all(|l| !l.prominent));
        for l in &p.lines {
            assert_eq!(l.is_new, l.capability != "db.read", "{}", l.capability);
        }
        let first = prompt_for(&["db.read"]);
        assert!(!first.reprompt);
        assert!(first.lines.iter().all(|l| !l.is_new));
    }

    #[test]
    fn names_and_descriptions_are_sanitised() {
        let mut m = fixture_manifest("acme-sync", "1.0.0", &["db.read"], false);
        m.name = format!("Evil\u{202E}Name\u{7}{}", "x".repeat(200));
        m.description = Some(format!("a\u{2066}b\n{}", "é".repeat(2000)));
        let p = GrantPrompt::new(
            &m,
            fixture_key(&m),
            &GrantStatus::NeedsPrompt { previous: None },
        );
        assert!(p.name.starts_with("Evil\u{FFFD}Name\u{FFFD}"));
        assert_eq!(p.name.chars().count(), 80);
        assert_eq!(p.title, format!("Allow plugin {}?", p.name));
        let d = p.description.unwrap();
        assert!(d.starts_with("a\u{FFFD}b\u{FFFD}"));
        assert!(d.len() <= 1024);
        assert!(!d.chars().any(char::is_control));
    }

    #[test]
    fn contributions_are_counted_without_zeros() {
        let p = prompt_for(&["db.read"]);
        assert_eq!(p.contributions, ["3 commands"]);
        let m = fixture_manifest("acme-sync", "1.0.0", &["db.read"], false);
        let none = GrantPrompt::new(
            &m,
            fixture_key(&m),
            &GrantStatus::NeedsPrompt { previous: None },
        );
        assert_eq!(none.contributions.len(), 0);
    }

    #[test]
    fn counts_are_pluralised() {
        assert_eq!(count(0, "panel", "panels"), None);
        assert_eq!(count(1, "panel", "panels").as_deref(), Some("1 panel"));
        assert_eq!(count(2, "panel", "panels").as_deref(), Some("2 panels"));
    }

    #[test]
    fn a_problem_disables_approve() {
        let m = fixture_manifest("acme-sync", "1.0.0", &["db.read"], false);
        let p = GrantPrompt::new(
            &m,
            fixture_key(&m),
            &GrantStatus::Unavailable {
                problem: "damaged".into(),
            },
        );
        assert!(!p.can_approve());
        assert_eq!(p.problem.as_deref(), Some("damaged"));
        assert!(prompt_for(&["db.read"]).can_approve());
    }

    #[test]
    fn only_plain_escape_is_bound_and_it_denies() {
        let key = |s| GrantPrompt::key_command(&KeyChord::parse(s, Platform::Other).unwrap());
        assert_eq!(key("escape"), Some(GrantCommand::Deny));
        assert_eq!(key("ctrl+escape"), None);
        assert_eq!(key("shift+escape"), None);
        for k in ["enter", "ctrl+enter", "cmd+enter", "space", "y", "a", "tab"] {
            assert_eq!(key(k), None, "{k}");
        }
    }

    #[test]
    fn the_sample_shows_the_reprompt_state() {
        let p = sample_prompt();
        assert!(p.reprompt);
        assert_eq!(p.lines.len(), 5);
        assert!(p.lines.iter().any(|l| l.is_new));
        assert!(p.lines.iter().any(|l| !l.is_new));
        assert!(p.lines[0].prominent);
    }
}
