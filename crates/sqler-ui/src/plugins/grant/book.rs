// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The book of grants: the typed view over `plugin-grants.toml` (ADR-0059).
//!
//! `sqler-config` stores validated strings; this module parses them into [`GrantKey`] and
//! [`Capability`] and decides what a lookup answers. Anything unclear denies.

use std::str::FromStr as _;
use std::sync::RwLock;

use sqler_config::{ConfigDir, ConfigError, GrantDecision, GrantRecord, GrantsLoad, PluginGrants};
use sqler_plugin_api::{
    Capability, CapabilitySet, ComponentDigest, GrantKey, GrantKeyBuildError, GrantSource,
    Manifest, PluginId,
};
use thiserror::Error;

use super::GrantCommand;
use super::prompt::GrantPrompt;

/// What the book knows about one plugin key.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum GrantStatus {
    /// The user approved exactly this key, with these capabilities.
    Approved(CapabilitySet),
    /// The user denied exactly this key.
    Denied,
    /// Nothing is decided for this key; `previous` is what an older key of the plugin had approved.
    NeedsPrompt {
        /// The capabilities approved under the plugin's earlier key, if they parse.
        previous: Option<CapabilitySet>,
    },
    /// The store is damaged, newer or unreadable: nothing is granted and nothing can be approved.
    Unavailable {
        /// Why, in one line.
        problem: String,
    },
}

/// Why a decision or a revoke did not happen.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum GrantBookError {
    /// The store is locked (damaged, newer or unreadable) or the decision does not fit it.
    #[error("plugin permissions are unavailable: {0}")]
    Unavailable(String),
    /// The file could not be written; nothing was granted.
    #[error("could not save plugin permissions: {0}")]
    Save(#[from] ConfigError),
    /// The decision of the plugin changed (another instance denied, revoked or approved it) after
    /// the dialog was built. Nothing was saved; build the prompt again from [`GrantBook::prompt`]
    /// so the user answers what is on disk now.
    #[error("the permissions of this plugin changed while the dialog was open; review them again")]
    StaleDecision,
    /// An earlier panic poisoned the lock; reads deny and writes are refused.
    #[error("plugin permissions are unavailable after an internal error")]
    Poisoned,
}

struct State {
    grants: PluginGrants,
    /// Set when the file could not be used; the book then denies everything.
    locked: Option<String>,
}

/// The grants of the user, read from and saved to `plugin-grants.toml`.
///
/// Only a saved decision counts: [`GrantBook::decide`] changes memory after the file is written.
/// Each change is applied to the file as it is on disk at that moment (reload, change one record,
/// save), so a denial or revoke written by another instance is not undone by an unrelated change.
/// Reads (`status`, `approved`) answer from memory until the next change or reopen.
pub struct GrantBook {
    dir: Option<ConfigDir>,
    state: RwLock<State>,
}

impl std::fmt::Debug for GrantBook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GrantBook").finish_non_exhaustive()
    }
}

impl GrantBook {
    /// Loads the book from `dir`. A damaged, newer or unreadable file locks the book and is
    /// never rewritten.
    #[must_use]
    pub fn open(dir: ConfigDir) -> Self {
        let (grants, locked) = match dir.load_plugin_grants() {
            GrantsLoad::Missing => (PluginGrants::new(), None),
            GrantsLoad::Loaded(g) => (g, None),
            GrantsLoad::Invalid(why) => (PluginGrants::new(), Some(why)),
            GrantsLoad::Newer(n) => (
                PluginGrants::new(),
                Some(format!("written by a newer version of SQLer (format {n})")),
            ),
        };
        Self {
            dir: Some(dir),
            state: RwLock::new(State { grants, locked }),
        }
    }

    /// A book that is not backed by a file, for tests and scenes.
    #[must_use]
    pub fn in_memory() -> Self {
        Self {
            dir: None,
            state: RwLock::new(State {
                grants: PluginGrants::new(),
                locked: None,
            }),
        }
    }

    /// Re-reads `plugin-grants.toml`, so a decision or revoke made by another instance applies now. `true` when what
    /// the book answers changed. A damaged or newer file locks the book (nothing is granted); a file that is valid
    /// again unlocks it. Blocking file I/O: call it off the UI thread. A book without a file never changes.
    pub fn reload(&self) -> bool {
        let Some(dir) = &self.dir else {
            return false;
        };
        let (grants, locked) = match dir.load_plugin_grants() {
            GrantsLoad::Missing => (PluginGrants::new(), None),
            GrantsLoad::Loaded(g) => (g, None),
            GrantsLoad::Invalid(why) => (PluginGrants::new(), Some(why)),
            GrantsLoad::Newer(n) => (
                PluginGrants::new(),
                Some(format!("written by a newer version of SQLer (format {n})")),
            ),
        };
        let Ok(mut state) = self.state.write() else {
            return false;
        };
        let changed = state.grants != grants || state.locked != locked;
        state.grants = grants;
        state.locked = locked;
        changed
    }

    /// What the book says about `key`.
    #[must_use]
    pub fn status(&self, key: &GrantKey) -> GrantStatus {
        let Ok(state) = self.state.read() else {
            return GrantStatus::Unavailable {
                problem: GrantBookError::Poisoned.to_string(),
            };
        };
        if let Some(problem) = &state.locked {
            return GrantStatus::Unavailable {
                problem: problem.clone(),
            };
        }
        status_in(&state.grants, key)
    }

    /// The dialog model for `manifest` and its component, from what the book says now. Use it
    /// again after [`GrantBookError::StaleDecision`].
    ///
    /// # Errors
    ///
    /// [`GrantKeyBuildError`] when the digest does not fit the manifest.
    pub fn prompt(
        &self,
        manifest: &Manifest,
        component: Option<&ComponentDigest>,
    ) -> Result<GrantPrompt, GrantKeyBuildError> {
        let key = manifest.grant_key(component)?;
        GrantPrompt::new(manifest, component, &self.status(&key))
    }

    /// Records the answer to `prompt` and saves it. Nothing changes if the save fails.
    ///
    /// Only what the prompt derived from its manifest is stored: its key and its capability set.
    /// The record of the plugin on disk must still be what the prompt was built from; if another
    /// instance changed it meanwhile, the answer is refused.
    ///
    /// # Errors
    ///
    /// [`GrantBookError::Unavailable`] when the book is locked or the prompt has a problem,
    /// [`GrantBookError::StaleDecision`] when the plugin's record changed since the prompt was
    /// built, [`GrantBookError::Save`] when the file cannot be written,
    /// [`GrantBookError::Poisoned`] after an internal error.
    pub fn decide(
        &self,
        prompt: &GrantPrompt,
        command: GrantCommand,
    ) -> Result<GrantStatus, GrantBookError> {
        if let Some(problem) = prompt.problem() {
            return Err(GrantBookError::Unavailable(problem.to_owned()));
        }
        let key = prompt.key();
        let record = match command {
            GrantCommand::Approve => GrantRecord::approved(
                key.to_string(),
                prompt
                    .capabilities()
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
            ),
            _ => GrantRecord::denied(key.to_string()),
        }
        .map_err(|e| GrantBookError::Unavailable(e.to_string()))?;

        let mut state = self.state.write().map_err(|_| GrantBookError::Poisoned)?;
        if let Some(problem) = &state.locked {
            return Err(GrantBookError::Unavailable(problem.clone()));
        }
        let mut next = self.fresh(&mut state)?;
        if status_in(&next, key) != *prompt.basis() {
            // Show the user what is on disk now on the next prompt.
            state.grants = next;
            return Err(GrantBookError::StaleDecision);
        }
        next.record(record);
        self.save(&next)?;
        state.grants = next;
        Ok(match command {
            GrantCommand::Approve => GrantStatus::Approved(prompt.capabilities().clone()),
            _ => GrantStatus::Denied,
        })
    }

    /// Turns the approval of `plugin` into a denial. `false` when it had none.
    ///
    /// # Errors
    ///
    /// As [`GrantBook::decide`].
    pub fn revoke(&self, plugin: &PluginId) -> Result<bool, GrantBookError> {
        let mut state = self.state.write().map_err(|_| GrantBookError::Poisoned)?;
        if let Some(problem) = &state.locked {
            return Err(GrantBookError::Unavailable(problem.clone()));
        }
        let mut next = self.fresh(&mut state)?;
        if !next.revoke(plugin.as_str()) {
            state.grants = next;
            return Ok(false);
        }
        self.save(&next)?;
        state.grants = next;
        Ok(true)
    }

    /// The grants as they are on disk right now, so a change made by another `SQLer` instance since
    /// `open` is kept: a decision applies to the fresh file, never to the snapshot taken at `open`.
    /// A file that turned damaged or newer locks the book.
    fn fresh(&self, state: &mut State) -> Result<PluginGrants, GrantBookError> {
        let Some(dir) = &self.dir else {
            return Ok(state.grants.clone());
        };
        let why = match dir.load_plugin_grants() {
            GrantsLoad::Missing => return Ok(PluginGrants::new()),
            GrantsLoad::Loaded(g) => return Ok(g),
            GrantsLoad::Invalid(why) => why,
            GrantsLoad::Newer(n) => format!("written by a newer version of SQLer (format {n})"),
        };
        state.grants = PluginGrants::new();
        state.locked = Some(why.clone());
        Err(GrantBookError::Unavailable(why))
    }

    fn save(&self, grants: &PluginGrants) -> Result<(), GrantBookError> {
        match &self.dir {
            Some(dir) => Ok(dir.save_plugin_grants(grants)?),
            None => Ok(()),
        }
    }
}

/// What `grants` say about `key`.
fn status_in(grants: &PluginGrants, key: &GrantKey) -> GrantStatus {
    let Some(record) = grants.for_plugin(key.id().as_str()) else {
        return GrantStatus::NeedsPrompt { previous: None };
    };
    if record.key() == key.to_string() {
        return match record.decision() {
            GrantDecision::Approved => match parse_caps(record) {
                Some(set) => GrantStatus::Approved(set),
                None => GrantStatus::NeedsPrompt { previous: None },
            },
            _ => GrantStatus::Denied,
        };
    }
    let previous = match record.decision() {
        GrantDecision::Approved => parse_caps(record),
        _ => None,
    };
    GrantStatus::NeedsPrompt { previous }
}

fn parse_caps(record: &GrantRecord) -> Option<CapabilitySet> {
    let mut set = CapabilitySet::new();
    for text in record.capabilities() {
        set.insert(Capability::from_str(text).ok()?);
    }
    Some(set)
}

impl GrantSource for GrantBook {
    fn approved(&self, key: &GrantKey) -> Option<CapabilitySet> {
        match self.status(key) {
            GrantStatus::Approved(set) => Some(set),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::grant::prompt::{fixture_digest, fixture_key, fixture_manifest};

    struct Scratch(std::path::PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scratch() -> (Scratch, ConfigDir) {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!("sqler-ui-{}-grants-{n}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        let d = ConfigDir::new(&p);
        (Scratch(p), d)
    }

    fn manifest(version: &str, caps: &[&str]) -> sqler_plugin_api::Manifest {
        fixture_manifest("acme-sync", version, caps, true)
    }

    fn prompt(m: &sqler_plugin_api::Manifest, book: &GrantBook) -> GrantPrompt {
        book.prompt(m, fixture_digest(m).as_ref()).unwrap()
    }

    fn set(caps: &[&str]) -> CapabilitySet {
        let mut s = CapabilitySet::new();
        for c in caps {
            s.insert(c.parse().unwrap());
        }
        s
    }

    #[test]
    fn nothing_decided_needs_a_prompt() {
        let (_s, dir) = scratch();
        let book = GrantBook::open(dir);
        let m = manifest("1.0.0", &["db.read"]);
        let key = fixture_key(&m);
        assert_eq!(
            book.status(&key),
            GrantStatus::NeedsPrompt { previous: None }
        );
        assert_eq!(book.approved(&key), None);
    }

    #[test]
    fn approve_is_kept_across_a_reopen() {
        let (_s, dir) = scratch();
        let book = GrantBook::open(dir.clone());
        let m = manifest("1.0.0", &["db.read", "net.http:example.com"]);
        let p = prompt(&m, &book);
        let got = book.decide(&p, GrantCommand::Approve).unwrap();
        let want = set(&["db.read", "net.http:example.com"]);
        assert_eq!(got, GrantStatus::Approved(want.clone()));
        let key = fixture_key(&m);
        assert_eq!(book.approved(&key), Some(want.clone()));
        let again = GrantBook::open(dir);
        assert_eq!(again.status(&key), GrantStatus::Approved(want));
    }

    #[test]
    fn a_changed_key_asks_again_with_the_previous_set() {
        let (_s, dir) = scratch();
        let book = GrantBook::open(dir);
        let m = manifest("1.0.0", &["db.read"]);
        book.decide(&prompt(&m, &book), GrantCommand::Approve)
            .unwrap();
        let before = set(&["db.read"]);
        for changed in [
            manifest("1.1.0", &["db.read"]),
            manifest("1.0.0", &["db.read", "db.write"]),
        ] {
            let key = fixture_key(&changed);
            assert_eq!(
                book.status(&key),
                GrantStatus::NeedsPrompt {
                    previous: Some(before.clone())
                }
            );
            assert_eq!(book.approved(&key), None);
        }
        // The same manifest with another component: another key.
        let swapped = m
            .grant_key(Some(&sqler_plugin_api::ComponentDigest::of(b"other")))
            .unwrap();
        assert_eq!(
            book.status(&swapped),
            GrantStatus::NeedsPrompt {
                previous: Some(before)
            }
        );
        assert_eq!(book.approved(&swapped), None);
    }

    #[test]
    fn deny_is_remembered_and_grants_nothing() {
        let (_s, dir) = scratch();
        let book = GrantBook::open(dir.clone());
        let m = manifest("1.0.0", &["db.read"]);
        let key = fixture_key(&m);
        assert_eq!(
            book.decide(&prompt(&m, &book), GrantCommand::Deny).unwrap(),
            GrantStatus::Denied
        );
        assert_eq!(book.status(&key), GrantStatus::Denied);
        assert_eq!(book.approved(&key), None);
        assert_eq!(GrantBook::open(dir).status(&key), GrantStatus::Denied);
    }

    #[test]
    fn revoke_turns_an_approval_into_a_denial() {
        let (_s, dir) = scratch();
        let book = GrantBook::open(dir.clone());
        let m = manifest("1.0.0", &["db.read"]);
        let key = fixture_key(&m);
        assert!(!book.revoke(m.id()).unwrap());
        book.decide(&prompt(&m, &book), GrantCommand::Approve)
            .unwrap();
        assert!(book.revoke(m.id()).unwrap());
        assert_eq!(book.status(&key), GrantStatus::Denied);
        assert_eq!(GrantBook::open(dir).status(&key), GrantStatus::Denied);
        assert!(!book.revoke(m.id()).unwrap());
    }

    #[test]
    fn a_damaged_file_locks_the_book_and_stays_untouched() {
        let (_s, dir) = scratch();
        let path = dir.plugin_grants_path();
        std::fs::write(&path, "this is = [not toml").unwrap();
        let book = GrantBook::open(dir);
        let m = manifest("1.0.0", &["db.read"]);
        let key = fixture_key(&m);
        let status = book.status(&key);
        assert!(matches!(status, GrantStatus::Unavailable { .. }));
        assert_eq!(book.approved(&key), None);
        let p = prompt(&m, &book);
        assert!(!p.can_approve());
        for c in [GrantCommand::Approve, GrantCommand::Deny] {
            assert!(matches!(
                book.decide(&p, c),
                Err(GrantBookError::Unavailable(_))
            ));
        }
        assert!(matches!(
            book.revoke(m.id()),
            Err(GrantBookError::Unavailable(_))
        ));
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "this is = [not toml"
        );
    }

    #[test]
    fn a_newer_file_locks_the_book() {
        let (_s, dir) = scratch();
        std::fs::write(dir.plugin_grants_path(), "version = 2\n").unwrap();
        let book = GrantBook::open(dir);
        let key = fixture_key(&manifest("1.0.0", &[]));
        assert!(matches!(book.status(&key), GrantStatus::Unavailable { .. }));
    }

    #[test]
    fn a_failed_save_grants_nothing() {
        let (s, dir) = scratch();
        let book = GrantBook::open(dir.clone());
        let m = manifest("1.0.0", &["db.read"]);
        let p = prompt(&m, &book);
        // A directory in the place of the file makes the decision fail (the reload finds an unusable file).
        std::fs::create_dir(dir.plugin_grants_path()).unwrap();
        assert!(book.decide(&p, GrantCommand::Approve).is_err());
        assert_eq!(book.approved(&fixture_key(&m)), None);
        assert!(matches!(
            book.status(&fixture_key(&m)),
            GrantStatus::Unavailable { .. }
        ));
        drop(s);
    }

    #[test]
    fn a_stored_capability_that_does_not_parse_needs_a_prompt() {
        let (_s, dir) = scratch();
        let m = manifest("1.0.0", &["db.read"]);
        let key = fixture_key(&m);
        let mut grants = PluginGrants::new();
        grants.record(GrantRecord::approved(key.to_string(), vec!["bogus.thing".into()]).unwrap());
        dir.save_plugin_grants(&grants).unwrap();
        let book = GrantBook::open(dir);
        assert_eq!(
            book.status(&key),
            GrantStatus::NeedsPrompt { previous: None }
        );
        assert_eq!(book.approved(&key), None);
    }

    #[test]
    fn the_in_memory_book_decides_without_a_file() {
        let book = GrantBook::in_memory();
        let m = manifest("1.0.0", &["db.read"]);
        book.decide(&prompt(&m, &book), GrantCommand::Approve)
            .unwrap();
        assert!(book.approved(&fixture_key(&m)).is_some());
    }

    #[test]
    fn a_denial_from_another_instance_is_not_flipped_back() {
        let (_s, dir) = scratch();
        let first = manifest("1.0.0", &["db.read"]);
        let second = fixture_manifest("other-plugin", "1.0.0", &["db.read"], true);
        let one = GrantBook::open(dir.clone());
        one.decide(&prompt(&first, &one), GrantCommand::Approve)
            .unwrap();
        let two = GrantBook::open(dir.clone());
        assert!(matches!(
            two.status(&fixture_key(&first)),
            GrantStatus::Approved(_)
        ));
        // Instance two revokes the first plugin; instance one, still holding it as approved, approves another.
        assert!(two.revoke(first.id()).unwrap());
        one.decide(&prompt(&second, &one), GrantCommand::Approve)
            .unwrap();
        let again = GrantBook::open(dir.clone());
        assert_eq!(again.status(&fixture_key(&first)), GrantStatus::Denied);
        assert!(matches!(
            again.status(&fixture_key(&second)),
            GrantStatus::Approved(_)
        ));
        // Instance two denies a third plugin; instance one approves a fourth; the denials stay.
        let third = fixture_manifest("third-plugin", "1.0.0", &["db.read"], true);
        two.decide(&prompt(&third, &two), GrantCommand::Deny)
            .unwrap();
        let fourth = fixture_manifest("fourth-plugin", "1.0.0", &["db.read"], true);
        one.decide(&prompt(&fourth, &one), GrantCommand::Approve)
            .unwrap();
        let last = GrantBook::open(dir);
        assert_eq!(last.status(&fixture_key(&third)), GrantStatus::Denied);
        assert_eq!(last.status(&fixture_key(&first)), GrantStatus::Denied);
    }

    #[test]
    fn a_file_damaged_since_open_locks_the_book_and_stays_untouched() {
        let (_s, dir) = scratch();
        let book = GrantBook::open(dir.clone());
        let m = manifest("1.0.0", &["db.read"]);
        std::fs::write(dir.plugin_grants_path(), "not = [valid").unwrap();
        let err = book.decide(&prompt(&m, &book), GrantCommand::Approve);
        assert!(matches!(err, Err(GrantBookError::Unavailable(_))));
        assert_eq!(
            std::fs::read_to_string(dir.plugin_grants_path()).unwrap(),
            "not = [valid"
        );
        assert!(matches!(
            book.status(&fixture_key(&m)),
            GrantStatus::Unavailable { .. }
        ));
    }

    #[test]
    fn a_decision_after_another_instance_denied_is_stale() {
        let (_s, dir) = scratch();
        let m = manifest("1.0.0", &["db.read", "net.http:example.com"]);
        let (one, two) = (GrantBook::open(dir.clone()), GrantBook::open(dir.clone()));
        let stale = prompt(&m, &one);
        two.decide(&prompt(&m, &two), GrantCommand::Deny).unwrap();
        let before = std::fs::read_to_string(dir.plugin_grants_path()).unwrap();
        assert!(matches!(
            one.decide(&stale, GrantCommand::Approve),
            Err(GrantBookError::StaleDecision)
        ));
        assert_eq!(
            std::fs::read_to_string(dir.plugin_grants_path()).unwrap(),
            before
        );
        let key = fixture_key(&m);
        assert_eq!(one.approved(&key), None);
        // The book now knows the denial, so the rebuilt prompt is current and can be answered.
        assert_eq!(one.status(&key), GrantStatus::Denied);
        let fresh = prompt(&m, &one);
        assert!(one.decide(&fresh, GrantCommand::Approve).is_ok());
        assert!(one.approved(&key).is_some());
    }

    #[test]
    fn a_decision_after_another_instance_revoked_is_stale() {
        let (_s, dir) = scratch();
        let v1 = manifest("1.0.0", &["db.read"]);
        let v2 = manifest("2.0.0", &["db.read", "net.http:example.com"]);
        let one = GrantBook::open(dir.clone());
        one.decide(&prompt(&v1, &one), GrantCommand::Approve)
            .unwrap();
        let two = GrantBook::open(dir);
        let reprompt = prompt(&v2, &one);
        assert!(reprompt.is_reprompt());
        assert!(two.revoke(v1.id()).unwrap());
        for command in [GrantCommand::Approve, GrantCommand::Deny] {
            assert!(matches!(
                one.decide(&reprompt, command),
                Err(GrantBookError::StaleDecision)
            ));
        }
        assert_eq!(one.approved(&fixture_key(&v2)), None);
    }

    #[test]
    fn an_unchanged_record_is_not_stale() {
        let (_s, dir) = scratch();
        let m = manifest("1.0.0", &["db.read"]);
        let (one, two) = (GrantBook::open(dir.clone()), GrantBook::open(dir));
        let p = prompt(&m, &one);
        // Another instance decides a different plugin meanwhile.
        let other = fixture_manifest("other-plugin", "1.0.0", &["db.read"], true);
        two.decide(&prompt(&other, &two), GrantCommand::Approve)
            .unwrap();
        assert!(one.decide(&p, GrantCommand::Approve).is_ok());
    }

    #[test]
    fn a_prompt_with_a_problem_is_never_stored() {
        let book = GrantBook::in_memory();
        let m = manifest("1.0.0", &["db.read"]);
        let p = prompt(&m, &book).with_problem("damaged");
        for c in [GrantCommand::Approve, GrantCommand::Deny] {
            assert!(matches!(
                book.decide(&p, c),
                Err(GrantBookError::Unavailable(_))
            ));
        }
        assert_eq!(book.approved(&fixture_key(&m)), None);
    }

    #[test]
    fn what_is_stored_is_the_key_and_capabilities_of_the_manifest() {
        let (_s, dir) = scratch();
        let book = GrantBook::open(dir.clone());
        let m = manifest("1.0.0", &["db.read", "net.http:example.com"]);
        book.decide(&prompt(&m, &book), GrantCommand::Approve)
            .unwrap();
        let text = std::fs::read_to_string(dir.plugin_grants_path()).unwrap();
        assert!(text.contains(&fixture_key(&m).to_string()), "{text}");
        assert!(text.contains("net.http:example.com"), "{text}");
    }

    #[test]
    fn reload_sees_another_instances_revoke_and_damage() {
        let (_s, dir) = scratch();
        let book = GrantBook::open(dir.clone());
        let other = GrantBook::open(dir.clone());
        let m = manifest("1.0.0", &["db.read"]);
        let key = fixture_key(&m);
        book.decide(&prompt(&m, &book), GrantCommand::Approve)
            .unwrap();
        assert!(!book.reload(), "nothing changed on disk");
        assert!(other.reload(), "the approval is new to the other instance");
        assert!(other.approved(&key).is_some());
        assert!(book.revoke(m.id()).unwrap());
        assert!(
            other.approved(&key).is_some(),
            "answers from memory until reload"
        );
        assert!(other.reload());
        assert_eq!(other.approved(&key), None);
        assert_eq!(other.status(&key), GrantStatus::Denied);
        assert!(!other.reload());
        std::fs::write(dir.plugin_grants_path(), "not = [toml").unwrap();
        assert!(other.reload());
        assert!(matches!(
            other.status(&key),
            GrantStatus::Unavailable { .. }
        ));
        assert_eq!(
            std::fs::read_to_string(dir.plugin_grants_path()).unwrap(),
            "not = [toml"
        );
        std::fs::write(dir.plugin_grants_path(), "version = 1\n").unwrap();
        assert!(other.reload(), "a valid file unlocks the book");
        assert!(matches!(
            other.status(&key),
            GrantStatus::NeedsPrompt { .. }
        ));
    }

    #[test]
    fn an_in_memory_book_never_reloads() {
        assert!(!GrantBook::in_memory().reload());
    }
}
