// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::fs::{read_optional, write_atomic};
use crate::{ConfigDir, ConfigError};

/// Which modifier the `mod` token stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    /// macOS: `mod` is `cmd`.
    MacOs,
    /// Every other platform: `mod` is `ctrl`.
    Other,
}

impl Platform {
    /// The platform this binary was built for.
    #[must_use]
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else {
            Self::Other
        }
    }
}

/// A command identifier of the form `category:verb-object`, for example `tab:close`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CommandId(String);

fn valid_segment(seg: &str) -> bool {
    let mut parts = seg.split('-');
    let first_ok = parts.next().is_some_and(|p| {
        let mut c = p.chars();
        c.next().is_some_and(|f| f.is_ascii_lowercase())
            && c.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    });
    first_ok
        && parts.all(|p| {
            !p.is_empty()
                && p.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        })
}

impl CommandId {
    /// Validates and wraps a command id.
    ///
    /// # Errors
    /// [`ConfigError::InvalidCommandId`] unless `id` is `seg:seg` where each
    /// segment matches `[a-z][a-z0-9]*(-[a-z0-9]+)*`.
    pub fn new(id: &str) -> Result<Self, ConfigError> {
        match id.split_once(':') {
            Some((a, b)) if valid_segment(a) && valid_segment(b) => Ok(Self(id.to_owned())),
            _ => Err(ConfigError::InvalidCommandId(id.to_owned())),
        }
    }

    /// The id as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CommandId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Modifier keys of a chord.
#[allow(clippy::struct_excessive_bools)] // one flag per modifier key
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Modifiers {
    /// The control key.
    pub ctrl: bool,
    /// The alt (option) key.
    pub alt: bool,
    /// The shift key.
    pub shift: bool,
    /// The command (super) key.
    pub cmd: bool,
}

/// A single-stroke key chord such as `ctrl+shift+p`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KeyChord {
    modifiers: Modifiers,
    key: String,
}

const PUNCTUATION: &str = r"`-=[]\;',./";
const NAMED_KEYS: &[&str] = &[
    "enter",
    "escape",
    "tab",
    "space",
    "backspace",
    "delete",
    "insert",
    "home",
    "end",
    "pageup",
    "pagedown",
    "up",
    "down",
    "left",
    "right",
];

fn canonical_key(token: &str) -> Option<String> {
    let lower = token.to_ascii_lowercase();
    let t = match lower.as_str() {
        "return" => "enter",
        "esc" => "escape",
        other => other,
    };
    let mut chars = t.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        let ok = c.is_ascii_lowercase() || c.is_ascii_digit() || PUNCTUATION.contains(c);
        return ok.then(|| t.to_owned());
    }
    if NAMED_KEYS.contains(&t) {
        return Some(t.to_owned());
    }
    let n: u8 = t.strip_prefix('f')?.parse().ok()?;
    ((1..=24).contains(&n) && t == format!("f{n}")).then(|| t.to_owned())
}

impl KeyChord {
    /// Parses a chord such as `mod+shift+p`; see the grammar in the user guide.
    ///
    /// # Errors
    /// [`ConfigError::InvalidKeyChord`] for an empty chord, an unknown token,
    /// a repeated modifier, a missing key or more than one key.
    pub fn parse(s: &str, p: Platform) -> Result<Self, ConfigError> {
        let err = |reason: String| ConfigError::InvalidKeyChord {
            chord: s.to_owned(),
            reason,
        };
        if s.trim().is_empty() {
            return Err(err("empty chord".to_owned()));
        }
        let tokens: Vec<&str> = s.split('+').map(str::trim).collect();
        let (key_tok, mod_toks) = tokens.split_last().unwrap_or((&"", &[]));
        let mut m = Modifiers::default();
        for tok in mod_toks {
            let lower = tok.to_ascii_lowercase();
            let slot = match lower.as_str() {
                "mod" if p == Platform::MacOs => &mut m.cmd,
                "ctrl" | "control" | "mod" => &mut m.ctrl,
                "alt" | "option" => &mut m.alt,
                "shift" => &mut m.shift,
                "cmd" | "super" => &mut m.cmd,
                "" => return Err(err("empty token".to_owned())),
                _ => {
                    return Err(err(format!(
                        "`{tok}` is not a modifier; a chord has exactly one key, last"
                    )));
                }
            };
            if *slot {
                return Err(err(format!("modifier `{tok}` is repeated")));
            }
            *slot = true;
        }
        let key = canonical_key(key_tok).ok_or_else(|| {
            if key_tok.is_empty() {
                err("missing key".to_owned())
            } else {
                err(format!("`{key_tok}` is not a known key"))
            }
        })?;
        Ok(Self { modifiers: m, key })
    }

    /// The modifiers of the chord.
    #[must_use]
    pub fn modifiers(&self) -> Modifiers {
        self.modifiers
    }

    /// The canonical lowercase key name.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }
}

impl fmt::Display for KeyChord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let m = self.modifiers;
        for (on, name) in [
            (m.ctrl, "ctrl"),
            (m.alt, "alt"),
            (m.shift, "shift"),
            (m.cmd, "cmd"),
        ] {
            if on {
                write!(f, "{name}+")?;
            }
        }
        f.write_str(&self.key)
    }
}

/// User keybinding overrides: each entry replaces that command's chords.
pub type KeybindingOverrides = BTreeMap<CommandId, Vec<KeyChord>>;

/// Two or more commands bound to the same chord.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Conflict {
    /// The shared chord.
    pub chord: KeyChord,
    /// The commands bound to it, sorted.
    pub commands: Vec<CommandId>,
}

/// Built-in bindings. `mod` is `cmd` on macOS and `ctrl` elsewhere.
const DEFAULTS: &[(&str, &[&str])] = &[
    ("workspace:open-command-palette", &["mod+shift+p"]),
    ("settings:open", &["mod+,"]),
    ("tab:close", &["mod+w"]),
    ("tab:next", &["ctrl+tab"]),
    ("tab:previous", &["ctrl+shift+tab"]),
    ("app:quit", &["mod+q"]),
];

fn dedup(chords: Vec<KeyChord>) -> Vec<KeyChord> {
    let mut out: Vec<KeyChord> = Vec::with_capacity(chords.len());
    for c in chords {
        if !out.contains(&c) {
            out.push(c);
        }
    }
    out
}

/// Effective command-to-chords bindings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keymap {
    map: BTreeMap<CommandId, Vec<KeyChord>>,
}

impl Keymap {
    /// The built-in bindings for `p`.
    #[must_use]
    pub fn defaults(p: Platform) -> Self {
        let map = DEFAULTS
            .iter()
            .filter_map(|(id, chords)| {
                let id = CommandId::new(id).ok()?;
                let chords = chords
                    .iter()
                    .map(|c| KeyChord::parse(c, p))
                    .collect::<Result<Vec<_>, _>>()
                    .ok()?;
                Some((id, chords))
            })
            .collect();
        Self { map }
    }

    /// Defaults with `overrides` applied: an entry replaces the command's
    /// chords and an empty list unbinds it.
    #[must_use]
    pub fn with_overrides(p: Platform, overrides: &KeybindingOverrides) -> Self {
        let mut km = Self::defaults(p);
        for (id, chords) in overrides {
            if chords.is_empty() {
                km.map.remove(id);
            } else {
                km.map.insert(id.clone(), dedup(chords.clone()));
            }
        }
        km
    }

    /// Chords bound to `id`; empty when unbound.
    #[must_use]
    pub fn chords_for(&self, id: &CommandId) -> &[KeyChord] {
        self.map.get(id).map_or(&[], Vec::as_slice)
    }

    /// Commands bound to `chord`, sorted.
    #[must_use]
    pub fn commands_for(&self, chord: &KeyChord) -> Vec<&CommandId> {
        self.map
            .iter()
            .filter(|(_, cs)| cs.contains(chord))
            .map(|(id, _)| id)
            .collect()
    }

    /// All bindings in command order.
    pub fn bindings(&self) -> impl Iterator<Item = (&CommandId, &[KeyChord])> {
        self.map.iter().map(|(id, cs)| (id, cs.as_slice()))
    }

    /// Chords bound to two or more commands, sorted by chord.
    #[must_use]
    pub fn conflicts(&self) -> Vec<Conflict> {
        let mut by_chord: BTreeMap<&KeyChord, Vec<&CommandId>> = BTreeMap::new();
        for (id, chords) in &self.map {
            for c in chords {
                by_chord.entry(c).or_default().push(id);
            }
        }
        by_chord
            .into_iter()
            .filter(|(_, ids)| ids.len() > 1)
            .map(|(chord, ids)| Conflict {
                chord: chord.clone(),
                commands: ids.into_iter().cloned().collect(),
            })
            .collect()
    }
}

#[derive(Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct KeybindingsFile {
    #[serde(default)]
    bindings: BTreeMap<String, Vec<String>>,
}

fn invalid(path: &Path, command: &str, reason: &dyn fmt::Display) -> ConfigError {
    ConfigError::Invalid {
        path: path.to_owned(),
        message: format!("{command}: {reason}"),
    }
}

impl ConfigDir {
    /// Loads user keybinding overrides; a missing file yields an empty map.
    ///
    /// # Errors
    /// [`ConfigError::Read`], [`ConfigError::Parse`], or
    /// [`ConfigError::Invalid`] naming the command whose id or chord is bad.
    pub fn load_keybinding_overrides(
        &self,
        p: Platform,
    ) -> Result<KeybindingOverrides, ConfigError> {
        let path = self.keybindings_path();
        let Some(text) = read_optional(&path)? else {
            return Ok(KeybindingOverrides::new());
        };
        let file: KeybindingsFile = toml::from_str(&text).map_err(|e| ConfigError::Parse {
            path: path.clone(),
            message: e.to_string(),
        })?;
        let mut out = KeybindingOverrides::new();
        for (name, chords) in file.bindings {
            let id = CommandId::new(&name).map_err(|e| invalid(&path, &name, &e))?;
            let chords = chords
                .iter()
                .map(|c| KeyChord::parse(c, p))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| invalid(&path, &name, &e))?;
            out.insert(id, dedup(chords));
        }
        Ok(out)
    }

    /// Writes `keybindings.toml` with canonical chords.
    ///
    /// # Errors
    /// [`ConfigError::Serialize`] or [`ConfigError::Write`].
    pub fn save_keybinding_overrides(&self, o: &KeybindingOverrides) -> Result<(), ConfigError> {
        let path = self.keybindings_path();
        let file = KeybindingsFile {
            bindings: o
                .iter()
                .map(|(id, cs)| (id.to_string(), cs.iter().map(ToString::to_string).collect()))
                .collect(),
        };
        let text = toml::to_string_pretty(&file).map_err(|e| ConfigError::Serialize {
            path: path.clone(),
            message: e.to_string(),
        })?;
        write_atomic(&path, &text)
    }

    /// The effective keymap: built-in defaults plus the user's overrides.
    ///
    /// # Errors
    /// As [`ConfigDir::load_keybinding_overrides`].
    pub fn load_keymap(&self, p: Platform) -> Result<Keymap, ConfigError> {
        Ok(Keymap::with_overrides(
            p,
            &self.load_keybinding_overrides(p)?,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAC: Platform = Platform::MacOs;
    const OTHER: Platform = Platform::Other;

    fn id(s: &str) -> CommandId {
        CommandId::new(s).unwrap()
    }

    fn chord(s: &str, p: Platform) -> KeyChord {
        KeyChord::parse(s, p).unwrap()
    }

    #[test]
    fn command_id_accepts() {
        for ok in [
            "settings:open",
            "query:run-selection",
            "app:quit",
            "a1:b-2-c",
        ] {
            assert_eq!(CommandId::new(ok).unwrap().as_str(), ok);
        }
        assert_eq!(id("tab:close").to_string(), "tab:close");
    }

    #[test]
    fn command_id_rejects() {
        for bad in [
            "",
            "open",
            ":open",
            "tab:",
            "Tab:close",
            "tab:Close",
            "1tab:close",
            "tab:-x",
            "tab:x-",
            "tab:x--y",
            "a:b:c",
            "tab close:x",
            "tab:close_all",
        ] {
            assert!(
                matches!(CommandId::new(bad), Err(ConfigError::InvalidCommandId(_))),
                "{bad}"
            );
        }
    }

    #[test]
    fn chord_aliases_and_case() {
        assert_eq!(
            chord("Control+Option+Super+Shift+RETURN", OTHER).to_string(),
            "ctrl+alt+shift+cmd+enter"
        );
        assert_eq!(chord("esc", OTHER).to_string(), "escape");
        assert_eq!(chord("F12", OTHER).key(), "f12");
        assert_eq!(chord("ctrl + p", OTHER), chord("ctrl+p", OTHER));
    }

    #[test]
    fn mod_depends_on_platform() {
        let m = chord("mod+p", MAC).modifiers();
        assert!(m.cmd && !m.ctrl);
        let o = chord("mod+p", OTHER).modifiers();
        assert!(o.ctrl && !o.cmd);
    }

    #[test]
    fn chord_rejects() {
        for bad in [
            "",
            "  ",
            "ctrl+",
            "+p",
            "ctrl+ctrl+p",
            "mod+ctrl+p",
            "ctrl+a+b",
            "ctrl",
            "ctrl+foo",
            "f0",
            "f25",
            "f01",
            "ctrl+pp",
            "ctrl+É",
            "ctrl++",
        ] {
            assert!(
                matches!(
                    KeyChord::parse(bad, OTHER),
                    Err(ConfigError::InvalidKeyChord { .. })
                ),
                "{bad}"
            );
        }
    }

    #[test]
    fn chord_keys_cover_grammar() {
        for k in [
            "a",
            "z",
            "0",
            "9",
            "`",
            "-",
            "=",
            "[",
            "]",
            "\\",
            ";",
            "'",
            ",",
            ".",
            "/",
            "enter",
            "tab",
            "space",
            "backspace",
            "delete",
            "insert",
            "home",
            "end",
            "pageup",
            "pagedown",
            "up",
            "down",
            "left",
            "right",
            "f1",
            "f24",
        ] {
            assert_eq!(chord(k, OTHER).key(), k);
        }
    }

    #[test]
    fn canonical_display_round_trips() {
        for s in ["ctrl+alt+shift+cmd+enter", "shift+f1", "cmd+,", "tab"] {
            let c = chord(s, OTHER);
            assert_eq!(c.to_string(), s);
            assert_eq!(chord(&c.to_string(), OTHER), c);
        }
        assert_eq!(
            chord("cmd+shift+ctrl+alt+x", OTHER).to_string(),
            "ctrl+alt+shift+cmd+x"
        );
    }

    #[test]
    fn defaults_complete_and_conflict_free() {
        for p in [MAC, OTHER] {
            let km = Keymap::defaults(p);
            assert_eq!(
                km.bindings().count(),
                DEFAULTS.len(),
                "a default entry was dropped"
            );
            assert_eq!(km.conflicts().len(), 0);
        }
        let quit = id("app:quit");
        assert_eq!(
            Keymap::defaults(MAC).chords_for(&quit),
            [chord("cmd+q", MAC)]
        );
        assert_eq!(
            Keymap::defaults(OTHER).chords_for(&quit),
            [chord("ctrl+q", OTHER)]
        );
    }

    #[test]
    fn override_replaces_and_empty_unbinds() {
        let mut o = KeybindingOverrides::new();
        o.insert(
            id("tab:close"),
            vec![chord("mod+k", OTHER), chord("f4", OTHER)],
        );
        o.insert(id("app:quit"), vec![]);
        o.insert(id("plugin:custom"), vec![chord("alt+x", OTHER)]);
        let km = Keymap::with_overrides(OTHER, &o);
        assert_eq!(
            km.chords_for(&id("tab:close")),
            [chord("ctrl+k", OTHER), chord("f4", OTHER)]
        );
        assert_eq!(km.chords_for(&id("app:quit")).len(), 0);
        assert_eq!(km.chords_for(&id("plugin:custom")).len(), 1);
        assert_eq!(km.chords_for(&id("tab:next")).len(), 1);
    }

    #[test]
    fn conflict_with_another_default_is_reported() {
        let mut o = KeybindingOverrides::new();
        o.insert(id("tab:close"), vec![chord("mod+q", OTHER)]);
        let km = Keymap::with_overrides(OTHER, &o);
        let conflicts = km.conflicts();
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].chord, chord("ctrl+q", OTHER));
        assert_eq!(conflicts[0].commands, vec![id("app:quit"), id("tab:close")]);
        assert_eq!(
            km.commands_for(&chord("ctrl+q", OTHER)),
            vec![&id("app:quit"), &id("tab:close")]
        );
        assert_eq!(km.commands_for(&chord("ctrl+9", OTHER)).len(), 0);
    }

    #[test]
    fn conflicts_sorted_by_chord() {
        let mut o = KeybindingOverrides::new();
        o.insert(id("x:one"), vec![chord("z", OTHER), chord("a", OTHER)]);
        o.insert(id("x:two"), vec![chord("a", OTHER), chord("z", OTHER)]);
        let c = Keymap::with_overrides(OTHER, &o).conflicts();
        let keys: Vec<_> = c.iter().map(|c| c.chord.to_string()).collect();
        assert_eq!(keys, ["a", "z"]);
    }

    #[test]
    fn duplicate_chords_in_override_deduplicated() {
        let mut o = KeybindingOverrides::new();
        o.insert(
            id("tab:close"),
            vec![chord("f4", OTHER), chord("f4", OTHER)],
        );
        let km = Keymap::with_overrides(OTHER, &o);
        assert_eq!(km.chords_for(&id("tab:close")).len(), 1);
        assert_eq!(km.conflicts().len(), 0);
    }
}
