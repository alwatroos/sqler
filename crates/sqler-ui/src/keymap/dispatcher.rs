// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `KeyDispatcher`: the effective chord to command table.

use std::collections::HashMap;

use sqler_config::{CommandId as ConfigCommandId, KeyChord, KeybindingOverrides, Keymap, Platform};

use super::chord::display_chord;
use crate::command::{CommandId, CommandRegistry};

/// Context keys handled outside the keymap (the sidebar's, the grid's). They never enter the
/// keymap and are only shown as hints; see [`KeyDispatcher::hint`].
pub(crate) const LOCAL_KEYS: &[(&str, &str)] = &[
    ("object:open-data", "enter"),
    ("object:show-menu", "shift+f10"),
    ("grid:copy", "mod+c"),
    ("grid:copy-with-headers", "mod+shift+c"),
];

/// Resolves key chords to registered commands.
///
/// Built once at startup from the default keymap, the user's overrides and the command registry.
/// Anything odd (a load error, an override for an unknown command, a shortcut bound twice) is
/// collected as a notice for the user.
#[derive(Debug)]
pub struct KeyDispatcher {
    platform: Platform,
    keymap: Keymap,
    by_chord: HashMap<KeyChord, CommandId>,
    notices: Vec<String>,
}

fn to_ui(id: &ConfigCommandId, registry: &CommandRegistry) -> Option<CommandId> {
    let id = CommandId::new(id.as_str()).ok()?;
    registry.get(&id).is_some().then_some(id)
}

impl KeyDispatcher {
    /// Builds the dispatcher. `loaded` is the result of loading `keybindings.toml`; an `Err` gives
    /// the default shortcuts and a notice.
    #[must_use]
    pub fn new(
        p: Platform,
        loaded: Result<KeybindingOverrides, String>,
        registry: &CommandRegistry,
    ) -> Self {
        let mut notices = Vec::new();
        let overrides = match loaded {
            Ok(o) => o,
            Err(msg) => {
                notices.push(format!(
                    "Could not load keybindings.toml, using the default shortcuts: {msg}"
                ));
                KeybindingOverrides::new()
            }
        };
        let keymap = Keymap::with_overrides(p, &overrides);

        for id in overrides.keys() {
            if to_ui(id, registry).is_none() {
                notices.push(format!("Keybinding for unknown command `{id}` is ignored"));
            }
        }

        let mut by_chord = HashMap::new();
        for (id, chords) in keymap.bindings() {
            if let Some(ui) = to_ui(id, registry) {
                for chord in chords {
                    by_chord.insert(chord.clone(), ui.clone());
                }
            }
        }

        for conflict in keymap.conflicts() {
            let registered: Vec<(&ConfigCommandId, CommandId)> = conflict
                .commands
                .iter()
                .filter_map(|id| to_ui(id, registry).map(|ui| (id, ui)))
                .collect();
            let winner = registered
                .iter()
                .find(|(id, _)| {
                    overrides
                        .get(*id)
                        .is_some_and(|chords| chords.contains(&conflict.chord))
                })
                .or_else(|| registered.first())
                .map(|(_, ui)| ui.clone());
            let names: Vec<&str> = conflict
                .commands
                .iter()
                .map(ConfigCommandId::as_str)
                .collect();
            let outcome = winner
                .as_ref()
                .map_or_else(|| "nothing".to_owned(), ToString::to_string);
            notices.push(format!(
                "Shortcut {} is bound to {}; it runs {outcome}",
                conflict.chord,
                names.join(", ")
            ));
            if let Some(w) = winner {
                by_chord.insert(conflict.chord.clone(), w);
            }
        }

        Self {
            platform: p,
            keymap,
            by_chord,
            notices,
        }
    }

    /// The command a chord runs, if any.
    #[must_use]
    pub fn resolve(&self, chord: &KeyChord) -> Option<&CommandId> {
        self.by_chord.get(chord)
    }

    /// The command's effective chords, without those lost in a conflict.
    fn live_chords(&self, id: &CommandId) -> Vec<&KeyChord> {
        let Ok(config_id) = ConfigCommandId::new(id.as_str()) else {
            return Vec::new();
        };
        // A chord lost in a conflict runs another command, so it is not shown.
        self.keymap
            .chords_for(&config_id)
            .iter()
            .filter(|c| self.by_chord.get(*c) == Some(id))
            .collect()
    }

    /// The command's chords as text (`shift+cmd+p, ctrl+k`), or `None` when it is unbound.
    #[must_use]
    pub fn shortcut_label(&self, id: &CommandId) -> Option<String> {
        let chords: Vec<String> = self
            .live_chords(id)
            .into_iter()
            .map(ToString::to_string)
            .collect();
        if chords.is_empty() {
            return None;
        }
        Some(chords.join(", "))
    }

    /// The command's first effective chord as a platform label (`⌘⇧P`, `Ctrl+Shift+P`), or
    /// `None` when it is unbound.
    #[must_use]
    pub fn shortcut_display(&self, id: &CommandId) -> Option<String> {
        let chords = self.live_chords(id);
        let first = chords.first()?;
        Some(display_chord(first, self.platform))
    }

    /// The hint to show next to a command: its shortcut from the keymap, else the fixed context
    /// key of [`LOCAL_KEYS`], else `None`.
    #[must_use]
    pub fn hint(&self, id: &CommandId) -> Option<String> {
        if let Some(display) = self.shortcut_display(id) {
            return Some(display);
        }
        let (_, keys) = LOCAL_KEYS.iter().find(|(c, _)| *c == id.as_str())?;
        let chord = KeyChord::parse(keys, self.platform).ok()?;
        Some(display_chord(&chord, self.platform))
    }

    /// Messages for the user about the keybinding setup; empty when all is well.
    #[must_use]
    pub fn notices(&self) -> &[String] {
        &self.notices
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::Command;

    const OTHER: Platform = Platform::Other;
    const MAC: Platform = Platform::MacOs;

    fn cid(s: &str) -> CommandId {
        CommandId::new(s).unwrap()
    }

    fn chord(s: &str) -> KeyChord {
        KeyChord::parse(s, OTHER).unwrap()
    }

    fn registry(ids: &[&str]) -> CommandRegistry {
        let mut r = CommandRegistry::new();
        for id in ids {
            r.register(Command::new(id, *id).unwrap()).unwrap();
        }
        r
    }

    fn base() -> CommandRegistry {
        registry(&[
            "workspace:open-command-palette",
            "tab:close",
            "tab:next",
            "tab:previous",
            "app:quit",
            "query:run",
            "query:stop",
        ])
    }

    fn ovr(entries: &[(&str, &[&str])]) -> KeybindingOverrides {
        entries
            .iter()
            .map(|(id, cs)| {
                (
                    ConfigCommandId::new(id).unwrap(),
                    cs.iter().map(|c| chord(c)).collect(),
                )
            })
            .collect()
    }

    #[test]
    fn defaults_on_both_platforms() {
        let r = base();
        let k = KeyDispatcher::new(MAC, Ok(KeybindingOverrides::new()), &r);
        let open = cid("workspace:open-command-palette");
        assert_eq!(
            k.resolve(&KeyChord::parse("cmd+shift+p", MAC).unwrap()),
            Some(&open)
        );
        let k = KeyDispatcher::new(OTHER, Ok(KeybindingOverrides::new()), &r);
        assert_eq!(k.resolve(&chord("ctrl+shift+p")), Some(&open));
        assert_eq!(k.resolve(&chord("ctrl+w")), Some(&cid("tab:close")));
        assert_eq!(k.notices().len(), 0);
    }

    #[test]
    fn override_moves_a_shortcut() {
        let k = KeyDispatcher::new(OTHER, Ok(ovr(&[("tab:close", &["ctrl+k"])])), &base());
        assert_eq!(k.resolve(&chord("ctrl+k")), Some(&cid("tab:close")));
        assert_eq!(k.resolve(&chord("ctrl+w")), None);
        assert_eq!(k.notices().len(), 0);
    }

    #[test]
    fn empty_override_unbinds() {
        let k = KeyDispatcher::new(OTHER, Ok(ovr(&[("tab:close", &[])])), &base());
        assert_eq!(k.resolve(&chord("ctrl+w")), None);
        assert_eq!(k.shortcut_label(&cid("tab:close")), None);
    }

    #[test]
    fn unknown_override_gives_a_notice() {
        let k = KeyDispatcher::new(OTHER, Ok(ovr(&[("nope:nothing", &["ctrl+k"])])), &base());
        assert_eq!(k.resolve(&chord("ctrl+k")), None);
        assert_eq!(
            k.notices(),
            ["Keybinding for unknown command `nope:nothing` is ignored"]
        );
    }

    #[test]
    fn default_for_unregistered_command_is_silent() {
        // `settings:open` is a default but is not registered here.
        let k = KeyDispatcher::new(OTHER, Ok(KeybindingOverrides::new()), &base());
        assert_eq!(k.resolve(&chord("ctrl+,")), None);
        assert_eq!(k.notices().len(), 0);
    }

    #[test]
    fn conflict_override_wins() {
        // The override wins over the default, whichever command sorts first.
        let k = KeyDispatcher::new(OTHER, Ok(ovr(&[("query:run", &["ctrl+w"])])), &base());
        assert_eq!(k.resolve(&chord("ctrl+w")), Some(&cid("query:run")));
        assert_eq!(
            k.notices(),
            ["Shortcut ctrl+w is bound to query:run, tab:close; it runs query:run"]
        );
        let k = KeyDispatcher::new(OTHER, Ok(ovr(&[("app:quit", &["ctrl+w"])])), &base());
        assert_eq!(k.resolve(&chord("ctrl+w")), Some(&cid("app:quit")));
    }

    #[test]
    fn shortcut_label_hides_lost_chords() {
        let k = KeyDispatcher::new(OTHER, Ok(ovr(&[("query:run", &["ctrl+w"])])), &base());
        assert_eq!(k.shortcut_label(&cid("query:run")).unwrap(), "ctrl+w");
        assert_eq!(k.shortcut_label(&cid("tab:close")), None);
    }

    #[test]
    fn conflict_without_override_winner_is_first_in_id_order() {
        let o = ovr(&[("query:stop", &["ctrl+k"]), ("query:run", &["ctrl+k"])]);
        let k = KeyDispatcher::new(OTHER, Ok(o), &base());
        // Both are overrides containing the chord: the first in ID order wins.
        assert_eq!(k.resolve(&chord("ctrl+k")), Some(&cid("query:run")));
        assert_eq!(k.notices().len(), 1);
    }

    #[test]
    fn conflict_notice_per_conflict_and_unregistered_loser() {
        // `settings:open` is unregistered: it never wins and the registered command runs.
        let o = ovr(&[
            ("tab:close", &["ctrl+,"]),
            ("tab:next", &["ctrl+q"]),
            ("app:quit", &["ctrl+q"]),
        ]);
        let k = KeyDispatcher::new(OTHER, Ok(o), &base());
        assert_eq!(k.resolve(&chord("ctrl+,")), Some(&cid("tab:close")));
        assert_eq!(k.notices().len(), 2);
        assert!(k.notices()[0].contains("ctrl+,"));
        assert!(k.notices()[0].ends_with("it runs tab:close"));
    }

    #[test]
    fn conflict_with_no_registered_command_runs_nothing() {
        let r = registry(&["query:run"]);
        let o = ovr(&[("tab:close", &["ctrl+q"])]);
        let k = KeyDispatcher::new(OTHER, Ok(o), &r);
        assert_eq!(k.resolve(&chord("ctrl+q")), None);
        assert!(k.notices().iter().any(|n| n.ends_with("it runs nothing")));
    }

    #[test]
    fn load_error_gives_defaults_and_a_notice() {
        let k = KeyDispatcher::new(OTHER, Err("bad file".into()), &base());
        assert_eq!(k.resolve(&chord("ctrl+w")), Some(&cid("tab:close")));
        assert_eq!(
            k.notices(),
            ["Could not load keybindings.toml, using the default shortcuts: bad file"]
        );
    }

    #[test]
    fn shortcut_labels() {
        let o = ovr(&[("tab:close", &["ctrl+k", "mod+shift+w"])]);
        let k = KeyDispatcher::new(OTHER, Ok(o), &base());
        assert_eq!(
            k.shortcut_label(&cid("tab:close")).unwrap(),
            "ctrl+k, ctrl+shift+w"
        );
        assert_eq!(
            k.shortcut_label(&cid("workspace:open-command-palette"))
                .unwrap(),
            "ctrl+shift+p"
        );
        assert_eq!(k.shortcut_label(&cid("query:run")), None);
    }

    #[test]
    fn shortcut_display_per_platform() {
        let r = base();
        let open = cid("workspace:open-command-palette");
        let k = KeyDispatcher::new(MAC, Ok(KeybindingOverrides::new()), &r);
        assert_eq!(k.shortcut_display(&open).unwrap(), "⌘⇧P");
        let k = KeyDispatcher::new(OTHER, Ok(KeybindingOverrides::new()), &r);
        assert_eq!(k.shortcut_display(&open).unwrap(), "Ctrl+Shift+P");
        assert_eq!(k.shortcut_display(&cid("app:nothing")), None);
    }

    #[test]
    fn shortcut_display_hides_lost_chords_and_shows_first_only() {
        let k = KeyDispatcher::new(OTHER, Ok(ovr(&[("query:run", &["ctrl+w"])])), &base());
        assert_eq!(k.shortcut_display(&cid("query:run")).unwrap(), "Ctrl+W");
        assert_eq!(k.shortcut_display(&cid("tab:close")), None);
        let o = ovr(&[("tab:close", &["ctrl+k", "mod+shift+w"])]);
        let k = KeyDispatcher::new(OTHER, Ok(o), &base());
        assert_eq!(k.shortcut_display(&cid("tab:close")).unwrap(), "Ctrl+K");
    }

    #[test]
    fn hint_prefers_the_keymap_then_local_keys() {
        let r = registry(&[
            "grid:copy",
            "grid:copy-as-json",
            "object:open-data",
            "query:run",
        ]);
        let o = ovr(&[("grid:copy-as-json", &["ctrl+j"])]);
        let k = KeyDispatcher::new(OTHER, Ok(o), &r);
        assert_eq!(k.hint(&cid("grid:copy-as-json")).unwrap(), "Ctrl+J");
        assert_eq!(k.hint(&cid("grid:copy")).unwrap(), "Ctrl+C");
        assert_eq!(k.hint(&cid("grid:nothing")), None);
        let k = KeyDispatcher::new(MAC, Ok(KeybindingOverrides::new()), &r);
        assert_eq!(k.hint(&cid("grid:copy")).unwrap(), "⌘C");
        let enter = k.hint(&cid("object:open-data")).unwrap();
        assert_eq!(enter, display_chord(&chord("enter"), MAC));
    }
}
