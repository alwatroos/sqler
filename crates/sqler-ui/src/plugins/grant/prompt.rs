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
    Capability, CapabilityKind, CapabilitySet, ComponentDigest, FsRoot, GrantKey,
    GrantKeyBuildError, HostPattern, Manifest,
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
    /// Whether the line needs attention: drawn with a warning and repeated in the always-visible
    /// block. Everything but the calm kinds (see `needs_attention`) is prominent.
    pub prominent: bool,
    /// Whether an earlier decision did not include it. Only set on a re-prompt.
    pub is_new: bool,
}

/// The model of the permission dialog.
///
/// A prompt cannot be forged: every field is private and the only constructor,
/// [`GrantPrompt::new`], takes the manifest and the component digest and derives the grant key
/// and the capability set itself. [`GrantBook::decide`](super::GrantBook::decide) stores only
/// what the prompt derived, so an approval is always for the manifest and component that were
/// shown.
///
/// ```compile_fail,E0451
/// use sqler_plugin_api::CapabilitySet;
/// use sqler_ui::plugins::grant::GrantPrompt;
///
/// // A prompt cannot be built by struct literal, whatever the key text or capability set.
/// let _ = GrantPrompt {
///     capabilities: CapabilitySet::new(),
///     problem: None,
/// };
/// ```
///
/// ```compile_fail,E0616
/// use sqler_ui::plugins::grant::GrantPrompt;
///
/// // A built prompt cannot be edited.
/// fn edit(mut p: GrantPrompt) {
///     p.problem = None;
/// }
/// ```
///
/// ```compile_fail,E0616
/// use sqler_ui::plugins::grant::GrantPrompt;
///
/// fn read_key_field(p: &GrantPrompt) -> String {
///     p.key.clone()
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantPrompt {
    key: GrantKey,
    plugin_id: String,
    version: String,
    name: String,
    description: Option<String>,
    title: String,
    capabilities: CapabilitySet,
    lines: Vec<CapabilityLine>,
    contributions: Vec<String>,
    reprompt: bool,
    problem: Option<String>,
    /// The status of the key the prompt was built from; `decide` refuses if the file moved on.
    basis: GrantStatus,
}

impl GrantPrompt {
    /// Builds the prompt for `manifest` with its component `component` (`None` for a declarative
    /// plugin), given what the book says about the resulting key.
    ///
    /// # Errors
    ///
    /// [`GrantKeyBuildError`] when the digest does not fit the manifest (a wasm plugin without a
    /// digest, or a declarative plugin with one).
    pub fn new(
        manifest: &Manifest,
        component: Option<&ComponentDigest>,
        status: &GrantStatus,
    ) -> Result<Self, GrantKeyBuildError> {
        let key = manifest.grant_key(component)?;
        Ok(Self::from_parts(
            key,
            manifest.name(),
            manifest.description(),
            manifest.capabilities(),
            contributions(manifest),
            status,
        ))
    }

    /// The shared tail of [`GrantPrompt::new`]: sanitises the display text and builds the capability rows.
    fn from_parts(
        key: GrantKey,
        name: &str,
        description: Option<&str>,
        capabilities: &CapabilitySet,
        contributions: Vec<String>,
        status: &GrantStatus,
    ) -> Self {
        let (previous, problem) = match status {
            GrantStatus::NeedsPrompt { previous } => (previous.as_ref(), None),
            GrantStatus::Unavailable { problem } => (None, Some(sanitize(problem))),
            _ => (None, None),
        };
        let name = clip_chars(&sanitize(name), NAME_MAX_CHARS);
        let mut lines: Vec<CapabilityLine> =
            capabilities.iter().map(|c| line(c, previous)).collect();
        // Stable: prominent rows first, set order inside each group.
        lines.sort_by_key(|l| !l.prominent);
        Self {
            plugin_id: sanitize(key.id().as_str()),
            version: sanitize(&key.version().to_string()),
            key,
            title: format!("Allow plugin {name}?"),
            name,
            description: description.map(|d| clip_bytes(&sanitize(d), DESCRIPTION_MAX_BYTES)),
            capabilities: capabilities.clone(),
            lines,
            contributions,
            reprompt: previous.is_some(),
            problem,
            basis: status.clone(),
        }
    }

    /// The grant key Approve and Deny are stored under.
    #[must_use]
    pub fn key(&self) -> &GrantKey {
        &self.key
    }

    /// What Approve grants: exactly the capabilities of the manifest the prompt was built from.
    #[must_use]
    pub fn capabilities(&self) -> &CapabilitySet {
        &self.capabilities
    }

    /// The status the prompt was built from.
    #[must_use]
    pub(crate) fn basis(&self) -> &GrantStatus {
        &self.basis
    }

    /// The plugin id, sanitised.
    #[must_use]
    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    /// The plugin version.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    /// The display name, sanitised.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The description, sanitised.
    #[must_use]
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    /// `Allow plugin <name>?`.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// The capabilities in words: attention lines first, then set order.
    #[must_use]
    pub fn lines(&self) -> &[CapabilityLine] {
        &self.lines
    }

    /// What the plugin adds, for example `3 commands`; zero counts are left out.
    #[must_use]
    pub fn contributions(&self) -> &[String] {
        &self.contributions
    }

    /// Whether the plugin had an earlier decision under another key.
    #[must_use]
    pub fn is_reprompt(&self) -> bool {
        self.reprompt
    }

    /// Why Approve is not possible, if it is not.
    #[must_use]
    pub fn problem(&self) -> Option<&str> {
        self.problem.as_deref()
    }

    /// Marks the prompt as not approvable, as a locked book does.
    #[cfg(test)]
    pub(crate) fn with_problem(mut self, problem: &str) -> Self {
        self.problem = Some(problem.to_owned());
        self
    }

    /// Whether Approve can be offered.
    #[must_use]
    pub fn can_approve(&self) -> bool {
        self.problem.is_none()
    }

    /// Whether `typed` confirms Approve from the keyboard: Approve can be offered and the text is exactly the plugin id
    /// (no trimming, no case folding). This is the only keyboard route to Approve.
    #[must_use]
    pub fn confirms(&self, typed: &str) -> bool {
        self.can_approve() && typed == self.plugin_id
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

/// Replaces control characters, line and paragraph separators and every format character
/// (Unicode category Cf: zero-width and joiner characters, bidirectional marks and isolates, the
/// soft hyphen, the byte order mark, the tag block) with U+FFFD.
fn sanitize(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_control() || is_format(c) || matches!(c, '\u{2028}' | '\u{2029}') {
                '\u{FFFD}'
            } else {
                c
            }
        })
        .collect()
}

/// Unicode general category Cf (format), by range.
fn is_format(c: char) -> bool {
    matches!(c,
        '\u{00AD}'
        | '\u{0600}'..='\u{0605}'
        | '\u{061C}'
        | '\u{06DD}'
        | '\u{070F}'
        | '\u{0890}'..='\u{0891}'
        | '\u{08E2}'
        | '\u{180E}'
        | '\u{200B}'..='\u{200F}'
        | '\u{202A}'..='\u{202E}'
        | '\u{2060}'..='\u{2064}'
        | '\u{2066}'..='\u{206F}'
        | '\u{FEFF}'
        | '\u{FFF9}'..='\u{FFFB}'
        | '\u{110BD}'
        | '\u{110CD}'
        | '\u{13430}'..='\u{1343F}'
        | '\u{1BCA0}'..='\u{1BCA3}'
        | '\u{1D173}'..='\u{1D17A}'
        | '\u{E0001}'
        | '\u{E0020}'..='\u{E007F}'
    )
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
    let c = m.contributes();
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
            "Provide a database driver that runs inside the plugin. The plugin receives the host, username and password of every connection that uses its driver",
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
    l.prominent = l.prominent || needs_attention(capability.kind());
    l.is_new = previous.is_some_and(|p| !p.contains(capability));
    l
}

/// Whether a capability of `kind` needs attention. Only drawing the plugin's own panels is quiet.
/// Everything else, including reading the catalog of the user's connections (`db.read`) and any
/// kind added later, is shown with a warning, whatever its argument.
fn needs_attention(kind: CapabilityKind) -> bool {
    !matches!(kind, CapabilityKind::UiPanel)
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
#[cfg(test)]
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
#[cfg(test)]
#[allow(clippy::expect_used)] // fixed fixture text
pub(crate) fn fixture_key(m: &Manifest) -> GrantKey {
    m.grant_key(fixture_digest(m).as_ref())
        .expect("fixture key")
}

/// The component digest of a fixture manifest (`None` without an `[entry]`).
#[cfg(test)]
pub(crate) fn fixture_digest(m: &Manifest) -> Option<sqler_plugin_api::ComponentDigest> {
    m.entry()
        .is_some()
        .then(|| sqler_plugin_api::ComponentDigest::of(b"wasm"))
}

/// The prompt of a fixture manifest for `status`.
#[cfg(test)]
#[allow(clippy::expect_used)] // fixed fixture text
pub(crate) fn fixture_prompt(m: &Manifest, status: &GrantStatus) -> GrantPrompt {
    GrantPrompt::new(m, fixture_digest(m).as_ref(), status).expect("fixture prompt")
}

/// The prompt of the `plugins-grant` scene: a re-prompt with a new, prominent set.
#[cfg(test)]
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
    fixture_prompt(
        &m,
        &GrantStatus::NeedsPrompt {
            previous: Some(previous),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqler_config::Platform;

    /// A prompt whose name and description bypass manifest validation (which refuses control and bidi text), so
    /// the prompt's own sanitising is exercised.
    fn hostile_prompt(m: &Manifest, name: &str, description: &str) -> GrantPrompt {
        GrantPrompt::from_parts(
            fixture_key(m),
            name,
            Some(description),
            m.capabilities(),
            contributions(m),
            &GrantStatus::NeedsPrompt { previous: None },
        )
    }

    fn prompt_for(caps: &[&str]) -> GrantPrompt {
        let m = fixture_manifest("acme-sync", "1.0.0", caps, true);
        fixture_prompt(&m, &GrantStatus::NeedsPrompt { previous: None })
    }

    fn only(caps: &str) -> CapabilityLine {
        let all: &[&str] = if caps == "db.write" {
            &["db.read", "db.write"]
        } else {
            &[caps]
        };
        let p = prompt_for(all);
        p.lines()
            .iter()
            .find(|l| l.capability == caps)
            .cloned()
            .unwrap()
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
        assert!(only("db.read").prominent);
        assert!(!only("ui.panel").prominent);
    }

    #[test]
    fn a_guest_driver_line_says_the_plugin_receives_the_credentials() {
        let l = only("driver.guest");
        assert!(l.prominent);
        for word in ["host", "username", "password", "every connection"] {
            assert!(l.summary.contains(word), "{word}: {}", l.summary);
        }
        // The built-in engine line is about the host's own engine and promises nothing about credentials.
        assert!(!only("driver.host:duckdb").summary.contains("password"));
    }

    #[test]
    fn punycode_is_shown_as_is_with_a_note() {
        let l = only("net.http:*.xn--bcher-kva.example");
        assert_eq!(l.detail.as_deref(), Some("*.xn--bcher-kva.example"));
        assert!(l.prominent);
        assert!(l.notes.iter().any(|n| n.contains("punycode")));
        assert!(l.notes.iter().any(|n| n.contains("every subdomain")));
        assert!(l.notes.iter().any(|n| n == "ports 80 and 443"));
        assert!(
            only("net.http:example.com:8080")
                .notes
                .contains(&"port 8080".to_owned())
        );
    }

    #[test]
    fn every_net_http_host_needs_attention_and_shows_its_host() {
        for c in [
            "net.http:attacker.example",
            "net.http:example.com:8080",
            "net.http:*.example.com",
        ] {
            let l = only(c);
            assert!(l.prominent, "{c}");
            assert_eq!(l.capability, c);
        }
    }

    #[test]
    fn only_the_calm_kinds_are_quiet() {
        assert!(!only("ui.panel").prominent);
        for c in [
            "db.read",
            "db.write",
            "ai.tool",
            "driver.guest",
            "driver.host:duckdb",
            "net.http:example.com",
            "fs.read:plugin",
            "fs.read:data",
            "fs.read:data/cache",
            "fs.read:home/Documents",
        ] {
            assert!(only(c).prominent, "{c}");
        }
    }

    #[test]
    fn db_read_with_a_web_host_is_not_calm() {
        let p = prompt_for(&["db.read", "net.http:attacker.example"]);
        assert_eq!(p.attention_count(), 2);
        assert_eq!(p.summary(), "2 permissions, 2 need attention");
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
        for host in ["localhost.example.com", "notlocalhost.com"] {
            let l = only(&format!("net.http:{host}"));
            assert!(
                !l.notes.iter().any(|n| n.contains("your own computer")),
                "{host}"
            );
        }
    }

    #[test]
    fn the_summary_counts_permissions_and_warnings() {
        let p = prompt_for(&["db.read", "ui.panel", "fs.read:home/Library/x"]);
        assert_eq!(p.summary(), "3 permissions, 2 need attention");
        let p = prompt_for(&["db.read", "fs.read:data", "fs.read:home/Library/x"]);
        assert_eq!(p.summary(), "3 permissions, 3 need attention");
        assert_eq!(
            prompt_for(&["db.read"]).summary(),
            "1 permission, 1 needs attention"
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
        assert!(only("fs.read:data").prominent);
        assert!(only("fs.read:plugin").prominent);
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
        let p = fixture_prompt(
            &m,
            &GrantStatus::NeedsPrompt {
                previous: Some(previous),
            },
        );
        assert!(p.is_reprompt());
        let prominent = p.lines().iter().take_while(|l| l.prominent).count();
        assert_eq!(prominent, 3);
        assert!(p.lines()[prominent..].iter().all(|l| !l.prominent));
        for l in p.lines() {
            assert_eq!(l.is_new, l.capability != "db.read", "{}", l.capability);
        }
        let first = prompt_for(&["db.read"]);
        assert!(!first.is_reprompt());
        assert!(first.lines().iter().all(|l| !l.is_new));
    }

    #[test]
    fn names_and_descriptions_are_sanitised() {
        let m = fixture_manifest("acme-sync", "1.0.0", &["db.read"], false);
        let p = hostile_prompt(
            &m,
            &format!("Evil\u{202E}Name\u{7}{}", "x".repeat(200)),
            &format!("a\u{2066}b\n{}", "é".repeat(2000)),
        );
        assert!(p.name().starts_with("Evil\u{FFFD}Name\u{FFFD}"));
        assert_eq!(p.name().chars().count(), 80);
        assert_eq!(p.title(), format!("Allow plugin {}?", p.name()));
        let d = p.description().unwrap();
        assert!(d.starts_with("a\u{FFFD}b\u{FFFD}"));
        assert!(d.len() <= 1024);
        assert!(!d.chars().any(char::is_control));
    }

    #[test]
    fn invisible_and_separator_characters_are_stripped() {
        let evil = [
            '\u{200B}',
            '\u{200C}',
            '\u{200D}',
            '\u{2060}',
            '\u{FEFF}',
            '\u{00AD}',
            '\u{2028}',
            '\u{2029}',
            '\u{E0041}',
            '\u{E007F}',
            '\u{202E}',
            '\u{2066}',
            '\u{061C}',
            '\u{0085}',
            '\u{1D173}',
        ];
        let m = fixture_manifest("acme-sync", "1.0.0", &["db.read"], false);
        let text: String = evil.iter().flat_map(|&c| ['a', c]).collect();
        let p = hostile_prompt(&m, &text, &text);
        let want = "a\u{FFFD}".repeat(evil.len());
        assert_eq!(p.name(), want);
        assert_eq!(p.description(), Some(want.as_str()));
        assert_eq!(sanitize("plain é 日本 ok"), "plain é 日本 ok");
    }

    #[test]
    fn a_prompt_for_a_mismatched_digest_is_refused() {
        let wasm = fixture_manifest("acme-sync", "1.0.0", &["db.read"], true);
        let declarative = fixture_manifest("acme-sync", "1.0.0", &["db.read"], false);
        let digest = ComponentDigest::of(b"x");
        let s = GrantStatus::NeedsPrompt { previous: None };
        assert!(GrantPrompt::new(&wasm, None, &s).is_err());
        assert!(GrantPrompt::new(&declarative, Some(&digest), &s).is_err());
        let p = GrantPrompt::new(&wasm, Some(&digest), &s).unwrap();
        assert_eq!(*p.key(), wasm.grant_key(Some(&digest)).unwrap());
        assert_eq!(p.capabilities(), wasm.capabilities());
    }

    #[test]
    fn contributions_are_counted_without_zeros() {
        let p = prompt_for(&["db.read"]);
        assert_eq!(p.contributions(), ["3 commands"]);
        let m = fixture_manifest("acme-sync", "1.0.0", &["db.read"], false);
        let none = fixture_prompt(&m, &GrantStatus::NeedsPrompt { previous: None });
        assert_eq!(none.contributions().len(), 0);
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
        let p = fixture_prompt(
            &m,
            &GrantStatus::Unavailable {
                problem: "damaged".into(),
            },
        );
        assert!(!p.can_approve());
        assert_eq!(p.problem(), Some("damaged"));
        assert!(prompt_for(&["db.read"]).can_approve());
    }

    #[test]
    fn confirms_needs_the_exact_id_and_no_problem() {
        let p = prompt_for(&["db.read"]);
        assert!(p.confirms("acme-sync"));
        for typed in [
            "",
            "acme",
            "Acme-Sync",
            " acme-sync",
            "acme-sync ",
            "acme-sync\n",
        ] {
            assert!(!p.confirms(typed), "{typed:?}");
        }
        let m = fixture_manifest("acme-sync", "1.0.0", &["db.read"], false);
        let broken = fixture_prompt(
            &m,
            &GrantStatus::Unavailable {
                problem: "damaged".into(),
            },
        );
        assert!(!broken.confirms("acme-sync"));
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
        assert!(p.is_reprompt());
        assert_eq!(p.lines().len(), 5);
        assert!(p.lines().iter().any(|l| l.is_new));
        assert!(p.lines().iter().any(|l| !l.is_new));
        assert!(p.lines()[0].prominent);
        assert_eq!(p.attention_count(), 5);
    }
}
