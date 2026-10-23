// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The import wizard on the workspace (ADR-0067): `Dialog::Import` and the effect round trips.
//!
//! The wizard asks for blocking work with [`Effect::Import`]; the view carries it out and answers
//! with [`Workspace::import_discovered`], [`Workspace::import_file_chosen`] and
//! [`Workspace::import_file_read`]. Every request has an id; an answer for any other id (the wizard
//! was closed or opened again meanwhile) is ignored. Nothing reaches the store before the user
//! confirms on the Secrets step, and the keychain is written only after `connections.toml` was
//! saved: the typed secrets leave as `SecretOp::Set` in [`Effect::SaveConnections`].

use std::path::PathBuf;

use sqler_import::{ImportError, SourceReport};

use super::state::{Effect, Workspace};
use crate::connections::{Dialog, ImportAction, ImportEffect, ImportStep, ImportWizard};

impl Workspace {
    /// The import wizard, while it is open.
    #[must_use]
    pub fn import_wizard(&self) -> Option<&ImportWizard> {
        match &self.dialog {
            Some(Dialog::Import(wizard)) => Some(wizard),
            _ => None,
        }
    }

    /// Applies a wizard action.
    pub(super) fn dispatch_import(&mut self, action: ImportAction) -> Effect {
        if action == ImportAction::Open {
            self.import_requests += 1;
            let request = self.import_requests;
            let opened = self.open_dialog(Dialog::Import(ImportWizard::new(request)));
            let discover = Effect::Import(ImportEffect::Discover { request });
            return match opened {
                Effect::None => discover,
                other => Effect::Batch(vec![other, discover]),
            };
        }
        let request = self.import_requests + 1;
        let Some(Dialog::Import(wizard)) = &mut self.dialog else {
            return Effect::None;
        };
        let before = wizard.step();
        let mut effect = None;
        let mut save = None;
        let mut close = false;
        match action {
            ImportAction::Open => {}
            ImportAction::SetCursor(to) => wizard.set_cursor(to),
            ImportAction::CursorUp => wizard.cursor_up(),
            ImportAction::CursorDown => wizard.cursor_down(),
            ImportAction::ChooseFile => effect = wizard.choose_file(request),
            ImportAction::ToggleEntry => wizard.toggle_entry(),
            ImportAction::SelectAll => wizard.select_all(),
            ImportAction::SelectNone => wizard.select_none(),
            ImportAction::SetSecret { index, kind, text } => wizard.set_secret(index, kind, text),
            ImportAction::Back => wizard.back(),
            ImportAction::Next => match wizard.step() {
                ImportStep::Source => effect = wizard.next_from_source(request),
                ImportStep::Preview => wizard.goto_secrets(),
                ImportStep::Secrets => save = Some(wizard.apply(&mut self.sidebar)),
                ImportStep::Summary => close = true,
            },
            ImportAction::Apply => {
                if wizard.step() == ImportStep::Secrets {
                    save = Some(wizard.apply(&mut self.sidebar));
                }
            }
        }
        let added = wizard.summary().is_some_and(|s| !s.added.is_empty());
        let after = wizard.step();
        if effect.is_some() {
            self.import_requests = request;
        }
        if close {
            self.dialog = None;
        } else if before != after {
            self.dialog_generation += 1;
        }
        match (effect, save) {
            (Some(effect), _) => Effect::Import(effect),
            (None, Some(secrets)) if added => Effect::SaveConnections { secrets },
            _ => Effect::None,
        }
    }

    /// The search of the default locations finished (from `ImportEffect::Discover`). `home` is
    /// the home directory, shown as `~`. An answer for another request is ignored.
    pub fn import_discovered(
        &mut self,
        request: u64,
        home: Option<PathBuf>,
        found: Vec<(&'static str, Vec<PathBuf>)>,
    ) {
        if let Some(Dialog::Import(wizard)) = &mut self.dialog
            && wizard.awaits(request)
            && wizard.scanning()
        {
            wizard.discovered(home, found);
        }
    }

    /// The open-file prompt answered (from `ImportEffect::PickFile`); `None` means it was
    /// dismissed. A path starts the read. An answer for another request is ignored.
    pub fn import_file_chosen(&mut self, request: u64, path: Option<PathBuf>) -> Effect {
        let next = self.import_requests + 1;
        let Some(Dialog::Import(wizard)) = &mut self.dialog else {
            return Effect::None;
        };
        if !wizard.awaits(request) {
            return Effect::None;
        }
        match wizard.file_chosen(next, path) {
            Some(effect) => {
                self.import_requests = next;
                Effect::Import(effect)
            }
            None => Effect::None,
        }
    }

    /// A file was read and parsed (from `ImportEffect::ReadFile`). On success the wizard plans
    /// against the connection list as it is now and moves to the preview; on failure it shows the
    /// error on the Source step. An answer for another request is ignored.
    pub fn import_file_read(&mut self, request: u64, result: Result<SourceReport, ImportError>) {
        let Some(Dialog::Import(wizard)) = &mut self.dialog else {
            return;
        };
        if !wizard.awaits(request) {
            return;
        }
        let before = wizard.step();
        wizard.read_done(result, self.sidebar.store(), &self.drivers);
        if wizard.step() != before {
            self.dialog_generation += 1;
        }
    }

    /// Screenshot scenes: opens the wizard with the search already answered. No I/O.
    #[cfg(any(test, feature = "screenshots"))]
    pub fn open_import_fixture(
        &mut self,
        home: Option<PathBuf>,
        found: Vec<(&'static str, Vec<PathBuf>)>,
    ) {
        self.dispatch_import(ImportAction::Open);
        if let Some(Dialog::Import(wizard)) = &mut self.dialog {
            wizard.discovered(home, found);
        }
    }

    /// Screenshot scenes: feeds the file `bytes` to the source `source` as if `path` was read.
    #[cfg(any(test, feature = "screenshots"))]
    pub fn import_fixture_report(&mut self, source: &'static str, path: PathBuf, bytes: &[u8]) {
        let result = sqler_import::source(source)
            .ok_or(ImportError::UnrecognisedFormat("unknown source"))
            .and_then(|s| s.parse(bytes));
        self.import_fixture_result(source, path, result);
    }

    /// Screenshot scenes: shows `error` as the result of reading a file of the source under the
    /// cursor.
    #[cfg(any(test, feature = "screenshots"))]
    pub fn import_fixture_error(&mut self, error: ImportError) {
        let source = self
            .import_wizard()
            .and_then(|w| w.options().into_iter().nth(w.cursor()))
            .map_or("pgadmin", |o| o.source);
        self.import_fixture_result(source, PathBuf::new(), Err(error));
    }

    #[cfg(any(test, feature = "screenshots"))]
    fn import_fixture_result(
        &mut self,
        source: &'static str,
        path: PathBuf,
        result: Result<SourceReport, ImportError>,
    ) {
        self.import_requests += 1;
        let request = self.import_requests;
        if let Some(Dialog::Import(wizard)) = &mut self.dialog {
            let _ = wizard.begin_read(request, source, path);
        }
        self.import_file_read(request, result);
    }
}

#[cfg(test)]
#[allow(clippy::assert_is_empty)]
mod tests {
    use std::path::Path;

    use sqler_config::{
        ConnectionSpec, ConnectionStore, ProfileSecretKind, SecretKind, SshAuth, SshProfile,
    };
    use sqler_driver_api::sqler_core::DriverId;

    use super::*;
    use crate::command::CommandId;
    use crate::connections::import::fixtures::{AZURE_DATA_STUDIO, BEEKEEPER};
    use crate::connections::{
        ConnectRequest, ConnectionAction, EntryBadge, SecretSource, SecretText, action_for,
    };
    use crate::sidebar::SidebarModel;
    use crate::workspace::SecretOp;

    const HOME: &str = "/home/test";

    fn driver(id: &str) -> DriverId {
        DriverId::new(id).unwrap()
    }

    fn drivers(ids: &[&str]) -> Vec<DriverId> {
        ids.iter().map(|id| driver(id)).collect()
    }

    fn store_with(names: &[&str]) -> ConnectionStore {
        let mut store = ConnectionStore::new();
        for (i, name) in names.iter().enumerate() {
            store
                .add_connection(
                    ConnectionSpec::new(*name, driver("sqlite"), format!("/data/{i}.db")),
                    None,
                )
                .unwrap();
        }
        store
    }

    fn ws_with(store: ConnectionStore, ids: &[&str]) -> Workspace {
        let mut w = Workspace::new(SidebarModel::from_store(store, None));
        w.set_drivers(drivers(ids));
        w
    }

    fn ws() -> Workspace {
        ws_with(
            store_with(&[]),
            &["postgres", "mysql", "sqlite", "sqlserver"],
        )
    }

    /// Runs a command id the way the view does: id -> action -> dispatch.
    fn run(w: &mut Workspace, id: &str) -> Effect {
        let action = action_for(&CommandId::new(id).unwrap(), w)
            .unwrap_or_else(|| panic!("{id} does not apply"));
        w.dispatch_connection(action)
    }

    fn wizard(w: &Workspace) -> &ImportWizard {
        w.import_wizard().expect("the wizard is open")
    }

    fn open(w: &mut Workspace) -> u64 {
        let Effect::Import(ImportEffect::Discover { request }) = run(w, "connection:import") else {
            panic!("expected Discover");
        };
        request
    }

    fn found(source: &'static str) -> Vec<(&'static str, Vec<PathBuf>)> {
        vec![
            ("pgadmin", vec![]),
            (
                source,
                vec![PathBuf::from(format!("{HOME}/exports/{source}.json"))],
            ),
        ]
    }

    /// Opens the wizard, answers the search and reads `bytes` as `source`; ends on Preview.
    fn preview(w: &mut Workspace, source: &'static str, bytes: &str) {
        let request = open(w);
        w.import_discovered(request, Some(PathBuf::from(HOME)), found(source));
        let options = wizard(w).options();
        let at = options
            .iter()
            .position(|o| o.source == source && o.path.is_some())
            .unwrap();
        w.dispatch_connection(ConnectionAction::Import(ImportAction::SetCursor(at)));
        let Effect::Import(ImportEffect::ReadFile {
            request,
            source: id,
            path,
        }) = run(w, "connection-import:next")
        else {
            panic!("expected ReadFile");
        };
        assert_eq!(id, source);
        assert!(path.starts_with(HOME));
        let report = sqler_import::source(source)
            .unwrap()
            .parse(bytes.as_bytes());
        w.import_file_read(request, report);
    }

    fn row_named<'a>(
        rows: &'a [crate::connections::EntryRow],
        name: &str,
    ) -> &'a crate::connections::EntryRow {
        rows.iter().find(|r| r.name == name).unwrap()
    }

    #[test]
    fn open_asks_for_the_search_and_the_commands_walk_every_step() {
        let mut w = ws();
        assert!(w.import_wizard().is_none());
        let request = open(&mut w);
        assert_eq!(wizard(&w).step(), ImportStep::Source);
        assert!(wizard(&w).scanning());
        assert!(wizard(&w).options().is_empty());

        w.import_discovered(request, Some(PathBuf::from(HOME)), found("beekeeper"));
        assert!(!wizard(&w).scanning());
        assert_eq!(wizard(&w).options().len(), 3);

        run(&mut w, "connection-import:cursor-down");
        let Effect::Import(ImportEffect::ReadFile {
            request, source, ..
        }) = run(&mut w, "connection-import:next")
        else {
            panic!("expected ReadFile");
        };
        assert_eq!(source, "beekeeper");
        assert!(wizard(&w).loading());
        assert!(!wizard(&w).can_next());
        let report = sqler_import::source("beekeeper")
            .unwrap()
            .parse(BEEKEEPER.as_bytes());
        w.import_file_read(request, report);

        let generation = w.dialog_generation();
        assert_eq!(wizard(&w).step(), ImportStep::Preview);
        assert_eq!(wizard(&w).rows().len(), 5);
        assert_eq!(wizard(&w).selected_count(), 3);
        assert_eq!(
            wizard(&w).file_label().as_deref(),
            Some("Beekeeper Studio - ~/exports/beekeeper.json")
        );

        assert!(matches!(
            run(&mut w, "connection-import:next"),
            Effect::None
        ));
        assert_eq!(wizard(&w).step(), ImportStep::Secrets);
        assert!(w.dialog_generation() > generation);
        assert_eq!(wizard(&w).secret_rows().len(), 4);

        let effect = run(&mut w, "connection-import:apply");
        assert!(matches!(effect, Effect::SaveConnections { .. }));
        assert_eq!(wizard(&w).step(), ImportStep::Summary);
        assert_eq!(wizard(&w).summary().unwrap().added.len(), 3);
        assert!(matches!(
            run(&mut w, "connection-import:next"),
            Effect::None
        ));
        assert!(w.import_wizard().is_none());
        assert_eq!(w.sidebar().store().connections().len(), 3);
    }

    #[test]
    fn the_new_connection_command_set_is_registered_and_needs_the_wizard() {
        let mut w = ws();
        for id in [
            "connection-import:cursor-up",
            "connection-import:cursor-down",
            "connection-import:choose-file",
            "connection-import:next",
            "connection-import:back",
            "connection-import:toggle-entry",
            "connection-import:select-all",
            "connection-import:select-none",
            "connection-import:apply",
        ] {
            assert_eq!(action_for(&CommandId::new(id).unwrap(), &w), None, "{id}");
        }
        assert_eq!(
            action_for(&CommandId::new("connection:import").unwrap(), &w),
            Some(ConnectionAction::Import(ImportAction::Open))
        );
        open(&mut w);
        assert_eq!(
            action_for(&CommandId::new("connection-import:apply").unwrap(), &w),
            Some(ConnectionAction::Import(ImportAction::Apply))
        );
    }

    #[test]
    fn a_source_with_no_file_asks_for_one() {
        let mut w = ws();
        let request = open(&mut w);
        w.import_discovered(request, None, found("beekeeper"));
        let options = wizard(&w).options();
        let ids: Vec<_> = options
            .iter()
            .map(|o| (o.source, o.path.is_some()))
            .collect();
        assert_eq!(
            ids,
            [
                ("pgadmin", false),
                ("beekeeper", true),
                ("azure-data-studio", false)
            ]
        );
        assert_eq!(options[0].label, "pgAdmin 4");
        let Effect::Import(ImportEffect::PickFile { request }) =
            run(&mut w, "connection-import:next")
        else {
            panic!("expected PickFile");
        };
        // Dismissing the prompt keeps the step.
        assert!(matches!(w.import_file_chosen(request, None), Effect::None));
        assert_eq!(wizard(&w).step(), ImportStep::Source);
        assert!(!wizard(&w).loading());

        let Effect::Import(ImportEffect::PickFile { request }) =
            run(&mut w, "connection-import:choose-file")
        else {
            panic!("expected PickFile");
        };
        let Effect::Import(ImportEffect::ReadFile {
            request: read,
            source,
            path,
        }) = w.import_file_chosen(request, Some(PathBuf::from("/x/servers.json")))
        else {
            panic!("expected ReadFile");
        };
        assert_eq!(source, "pgadmin");
        assert_eq!(path, Path::new("/x/servers.json"));
        assert!(wizard(&w).loading());
        assert_ne!(read, request);
        // The prompt's id no longer counts.
        assert!(matches!(
            w.import_file_chosen(request, Some(PathBuf::from("/y.json"))),
            Effect::None
        ));
    }

    #[test]
    fn a_failed_read_shows_a_fixed_text_until_the_cursor_moves() {
        let mut w = ws();
        let request = open(&mut w);
        w.import_discovered(request, None, found("beekeeper"));
        run(&mut w, "connection-import:cursor-down");
        let Effect::Import(ImportEffect::ReadFile { request, .. }) =
            run(&mut w, "connection-import:next")
        else {
            panic!("expected ReadFile");
        };
        w.import_file_read(request, Err(ImportError::NotUtf8));
        assert_eq!(wizard(&w).step(), ImportStep::Source);
        assert!(!wizard(&w).loading());
        assert_eq!(wizard(&w).error(), Some("The file is not UTF-8 text."));
        run(&mut w, "connection-import:cursor-up");
        assert_eq!(wizard(&w).error(), None);

        // A file of another tool gives the tool's name.
        run(&mut w, "connection-import:cursor-down");
        let Effect::Import(ImportEffect::ReadFile { request, .. }) =
            run(&mut w, "connection-import:next")
        else {
            panic!("expected ReadFile");
        };
        w.import_file_read(request, Err(ImportError::UnrecognisedFormat("no servers")));
        assert_eq!(
            wizard(&w).error(),
            Some("This is not a Beekeeper Studio file: no servers.")
        );
    }

    #[test]
    fn a_file_without_connections_stays_on_source() {
        let mut w = ws();
        preview(&mut w, "beekeeper", r#"{"connections": []}"#);
        assert_eq!(wizard(&w).step(), ImportStep::Source);
        assert_eq!(wizard(&w).error(), Some("The file holds no connections."));
    }

    #[test]
    fn late_answers_after_cancel_or_reopen_are_ignored() {
        let mut w = ws();
        let first = open(&mut w);
        run(&mut w, "connection:cancel");
        assert!(w.import_wizard().is_none());
        w.import_discovered(first, None, found("beekeeper"));
        w.import_file_read(first, Err(ImportError::NotUtf8));
        assert!(matches!(
            w.import_file_chosen(first, Some(PathBuf::from("/a.json"))),
            Effect::None
        ));
        assert!(w.import_wizard().is_none());

        // Reopened: the old search answer does not count, the new one does.
        let second = open(&mut w);
        assert_ne!(first, second);
        w.import_discovered(first, None, found("beekeeper"));
        assert!(wizard(&w).scanning());
        w.import_discovered(second, None, found("beekeeper"));
        assert!(!wizard(&w).scanning());

        // A read that was cancelled with the wizard does not touch the next one.
        run(&mut w, "connection-import:cursor-down");
        let Effect::Import(ImportEffect::ReadFile { request, .. }) =
            run(&mut w, "connection-import:next")
        else {
            panic!("expected ReadFile");
        };
        run(&mut w, "connection:cancel");
        let third = open(&mut w);
        w.import_file_read(request, Err(ImportError::NotAFile));
        assert_eq!(wizard(&w).error(), None);
        w.import_discovered(third, None, found("beekeeper"));
        assert_eq!(wizard(&w).error(), None);
    }

    #[test]
    fn cancel_at_every_step_changes_nothing() {
        let before = store_with(&["Existing"]);
        for steps in 0..4 {
            let mut w = ws_with(
                before.clone(),
                &["postgres", "mysql", "sqlite", "sqlserver"],
            );
            preview(&mut w, "beekeeper", BEEKEEPER);
            if steps >= 1 {
                run(&mut w, "connection-import:next");
            }
            if steps >= 2 {
                run(&mut w, "connection-import:back");
            }
            let effect = run(&mut w, "connection:cancel");
            assert!(matches!(effect, Effect::None), "step {steps}");
            assert!(w.import_wizard().is_none());
            assert_eq!(w.sidebar().store(), &before, "step {steps}");
        }
        // Cancel on Source too.
        let mut w = ws_with(before.clone(), &["sqlite"]);
        open(&mut w);
        assert!(matches!(run(&mut w, "connection:cancel"), Effect::None));
        assert_eq!(w.sidebar().store(), &before);
    }

    #[test]
    fn nothing_is_written_before_apply() {
        let before = store_with(&[]);
        let mut w = ws_with(
            before.clone(),
            &["postgres", "mysql", "sqlite", "sqlserver"],
        );
        preview(&mut w, "beekeeper", BEEKEEPER);
        run(&mut w, "connection-import:next");
        w.dispatch_connection(ConnectionAction::Import(ImportAction::SetSecret {
            index: 0,
            kind: ProfileSecretKind::DbPassword,
            text: SecretText::new("typed-value".to_owned()),
        }));
        assert!(wizard(&w).secret_is_set(0, ProfileSecretKind::DbPassword));
        assert_eq!(w.sidebar().store(), &before);
        // Back drops what was typed.
        run(&mut w, "connection-import:back");
        run(&mut w, "connection-import:next");
        assert!(!wizard(&w).secret_is_set(0, ProfileSecretKind::DbPassword));
    }

    #[test]
    fn a_name_clash_is_renamed_and_existing_connections_stay_byte_equal() {
        let before = store_with(&["Orders primary", "Reports"]);
        let mut w = ws_with(
            before.clone(),
            &["postgres", "mysql", "sqlite", "sqlserver"],
        );
        preview(&mut w, "beekeeper", BEEKEEPER);
        let rows = wizard(&w).rows();
        let orders = row_named(&rows, "Orders primary (2)");
        assert_eq!(orders.badge, EntryBadge::Rename);
        assert_eq!(orders.detail, "renamed from Orders primary");
        assert!(orders.selectable && orders.selected);
        assert_eq!(row_named(&rows, "Reports (2)").badge, EntryBadge::Rename);

        run(&mut w, "connection-import:next");
        run(&mut w, "connection-import:apply");
        let after = w.sidebar().store();
        for old in before.connections() {
            let kept = after.connection(old.id()).unwrap();
            assert_eq!(kept.spec(), old.spec());
        }
        assert_eq!(after.connections().len(), 5);
    }

    #[test]
    fn duplicates_and_skips_are_not_selectable() {
        let mut store = ConnectionStore::new();
        store
            .add_connection(
                ConnectionSpec::new(
                    "Mine",
                    driver("postgres"),
                    "postgres://db.example.test:5433/orders?user=alice",
                ),
                None,
            )
            .unwrap();
        let mut w = ws_with(store, &["postgres", "mysql", "sqlite", "sqlserver"]);
        preview(&mut w, "beekeeper", BEEKEEPER);
        let rows = wizard(&w).rows();
        let dup = row_named(&rows, "Orders primary");
        assert_eq!(dup.badge, EntryBadge::Duplicate);
        assert!(!dup.selectable && !dup.selected);
        assert!(dup.detail.starts_with("already saved as connection"));
        let duck = row_named(&rows, "Analytics file");
        assert_eq!(duck.badge, EntryBadge::Skipped);
        assert_eq!(duck.detail, "DuckDB is not in SQLer 1.0");
        assert!(!duck.selectable);

        // Toggling a non-selectable row does nothing; the cursor can sit on it.
        let at = rows
            .iter()
            .position(|r| r.name == "Orders primary")
            .unwrap();
        w.dispatch_connection(ConnectionAction::Import(ImportAction::SetCursor(at)));
        run(&mut w, "connection-import:toggle-entry");
        assert_eq!(wizard(&w).selected_count(), 2);
        // Select all picks only the entries that can be added.
        run(&mut w, "connection-import:select-none");
        assert_eq!(wizard(&w).selected_count(), 0);
        assert!(!wizard(&w).can_next());
        run(&mut w, "connection-import:next");
        assert_eq!(wizard(&w).step(), ImportStep::Preview);
        run(&mut w, "connection-import:select-all");
        assert_eq!(wizard(&w).selected_count(), 2);
    }

    #[test]
    fn toggling_an_entry_leaves_it_out_of_the_import() {
        let mut w = ws();
        preview(&mut w, "beekeeper", BEEKEEPER);
        let at = wizard(&w)
            .rows()
            .iter()
            .position(|r| r.name == "Reports")
            .unwrap();
        w.dispatch_connection(ConnectionAction::Import(ImportAction::SetCursor(at)));
        run(&mut w, "connection-import:toggle-entry");
        assert_eq!(wizard(&w).selected_count(), 2);
        run(&mut w, "connection-import:next");
        assert!(
            wizard(&w)
                .secret_rows()
                .iter()
                .all(|r| r.connection != "Reports")
        );
        run(&mut w, "connection-import:apply");
        let summary = wizard(&w).summary().unwrap();
        assert_eq!(summary.added, ["Orders primary", "Ledger"]);
        assert_eq!(summary.not_imported, 3);
        assert!(
            w.sidebar()
                .store()
                .connections()
                .iter()
                .all(|c| c.spec().name != "Reports")
        );
    }

    #[test]
    fn sqlserver_entries_ask_for_the_database_password_and_use_the_keychain() {
        for (source, bytes, name) in [
            ("beekeeper", BEEKEEPER, "Ledger"),
            ("azure-data-studio", AZURE_DATA_STUDIO, "Orders primary"),
        ] {
            let mut w = ws();
            preview(&mut w, source, bytes);
            let rows = wizard(&w).rows();
            let row = row_named(&rows, name);
            assert_eq!(row.badge, EntryBadge::Add, "{source}");
            assert!(
                !row.notes
                    .iter()
                    .any(|n| n.contains("keychain password yet")),
                "{source}: {:?}",
                row.notes
            );
            run(&mut w, "connection-import:next");
            let secret = wizard(&w)
                .secret_rows()
                .into_iter()
                .find(|r| r.connection == name && r.kind == ProfileSecretKind::DbPassword)
                .unwrap_or_else(|| panic!("{source}: no password row"));
            assert_eq!(secret.label, "Database password");
            w.dispatch_connection(ConnectionAction::Import(ImportAction::SetSecret {
                index: secret.index,
                kind: secret.kind,
                text: SecretText::new("typed-value".to_owned()),
            }));
            let Effect::SaveConnections { secrets } = run(&mut w, "connection-import:apply") else {
                panic!("expected SaveConnections");
            };
            let [SecretOp::Set(reference, _)] = secrets.as_slice() else {
                panic!("{source}: expected one Set, got {}", secrets.len());
            };
            assert_eq!(reference.kind(), SecretKind::DatabasePassword);
            assert!(reference.to_string().ends_with(":db-password"));
            let profile = w
                .sidebar()
                .store()
                .connections()
                .iter()
                .find(|c| c.spec().name == name)
                .unwrap();
            assert!(profile.spec().password_in_keychain);
            assert_eq!(profile.spec().driver.as_str(), "sqlserver");
            assert_eq!(reference.connection_id(), profile.id().as_str());
            let request = ConnectRequest::from_profile(profile).unwrap();
            assert!(matches!(request.db_password, SecretSource::Keychain(_)));
            let summary = wizard(&w).summary().unwrap();
            assert_eq!(summary.passwords_stored, 1, "{source}");
        }
    }

    #[test]
    fn sqlserver_is_skipped_without_the_driver_and_duckdb_is_not_in_1_0() {
        let mut w = ws_with(store_with(&[]), &["postgres", "mysql", "sqlite"]);
        preview(&mut w, "beekeeper", BEEKEEPER);
        let rows = wizard(&w).rows();
        let ledger = row_named(&rows, "Ledger");
        assert_eq!(ledger.badge, EntryBadge::Skipped);
        assert_eq!(ledger.detail, "not available in this build");
        assert!(!ledger.selectable);
        assert_eq!(
            row_named(&rows, "Analytics file").detail,
            "DuckDB is not in SQLer 1.0"
        );
    }

    #[test]
    fn typed_passwords_are_stored_and_empty_ones_are_skipped() {
        let mut w = ws();
        preview(&mut w, "beekeeper", BEEKEEPER);
        run(&mut w, "connection-import:next");
        let rows = wizard(&w).secret_rows();
        assert_eq!(
            rows.iter().map(|r| r.label).collect::<Vec<_>>(),
            [
                "Database password",
                "Database password",
                "Database password",
                "SSH password"
            ]
        );
        let set = |w: &mut Workspace, row: &crate::connections::SecretRow, text: &str| {
            w.dispatch_connection(ConnectionAction::Import(ImportAction::SetSecret {
                index: row.index,
                kind: row.kind,
                text: SecretText::new(text.to_owned()),
            }));
        };
        set(&mut w, &rows[0], "first-typed");
        set(&mut w, &rows[3], "second-typed");
        set(&mut w, &rows[3], "");
        assert!(wizard(&w).secret_is_set(rows[0].index, rows[0].kind));
        assert!(!wizard(&w).secret_is_set(rows[3].index, rows[3].kind));
        // A field that is not on the step is ignored.
        w.dispatch_connection(ConnectionAction::Import(ImportAction::SetSecret {
            index: 99,
            kind: ProfileSecretKind::SshPassphrase,
            text: SecretText::new("stray".to_owned()),
        }));
        assert!(!wizard(&w).secret_is_set(99, ProfileSecretKind::SshPassphrase));

        let Effect::SaveConnections { secrets } = run(&mut w, "connection-import:apply") else {
            panic!("expected SaveConnections");
        };
        assert_eq!(secrets.len(), 1);
        let summary = wizard(&w).summary().unwrap();
        assert_eq!(summary.passwords_stored, 1);
        assert_eq!(summary.passwords_skipped, 3);
        assert!(summary.failed.is_empty());
        // The typed values are gone from the wizard.
        assert!(!wizard(&w).secret_is_set(rows[0].index, rows[0].kind));
    }

    #[test]
    fn typed_values_never_show_in_debug_output() {
        let mut w = ws();
        preview(&mut w, "beekeeper", BEEKEEPER);
        run(&mut w, "connection-import:next");
        let rows = wizard(&w).secret_rows();
        let action = ConnectionAction::Import(ImportAction::SetSecret {
            index: rows[0].index,
            kind: rows[0].kind,
            text: SecretText::new("zq-typed-9981".to_owned()),
        });
        assert!(!format!("{action:?}").contains("zq-typed-9981"));
        w.dispatch_connection(action);
        assert!(!format!("{:?}", w.dialog()).contains("zq-typed-9981"));
        assert!(!format!("{:?}", wizard(&w).notices()).contains("zq-typed-9981"));
        let effect = run(&mut w, "connection-import:apply");
        // The effect's Debug shows the operation, not the secret.
        assert!(!format!("{effect:?}").contains("zq-typed-9981"));
        assert!(!format!("{:?}", w.dialog()).contains("zq-typed-9981"));
    }

    #[test]
    fn apply_outside_the_secrets_step_does_nothing() {
        let mut w = ws();
        preview(&mut w, "beekeeper", BEEKEEPER);
        assert!(matches!(
            run(&mut w, "connection-import:apply"),
            Effect::None
        ));
        assert_eq!(wizard(&w).step(), ImportStep::Preview);
        assert!(w.sidebar().store().connections().is_empty());
    }

    #[test]
    fn groups_come_with_the_connections() {
        let mut w = ws();
        preview(&mut w, "beekeeper", BEEKEEPER);
        let rows = wizard(&w).rows();
        assert_eq!(row_named(&rows, "Orders primary").group, "Shop / Staging");
        assert_eq!(row_named(&rows, "Ledger").group, "");
        run(&mut w, "connection-import:next");
        run(&mut w, "connection-import:apply");
        let store = w.sidebar().store();
        let names: Vec<_> = store.groups().iter().map(|g| g.name().to_owned()).collect();
        assert_eq!(names, ["Shop", "Staging"]);
    }

    #[test]
    fn back_walks_the_steps_and_summary_has_no_back() {
        let mut w = ws();
        preview(&mut w, "beekeeper", BEEKEEPER);
        run(&mut w, "connection-import:next");
        assert!(wizard(&w).can_back());
        run(&mut w, "connection-import:back");
        assert_eq!(wizard(&w).step(), ImportStep::Preview);
        run(&mut w, "connection-import:back");
        assert_eq!(wizard(&w).step(), ImportStep::Source);
        assert!(!wizard(&w).can_back());
        run(&mut w, "connection-import:back");
        assert_eq!(wizard(&w).step(), ImportStep::Source);
        run(&mut w, "connection-import:cursor-down");
        run(&mut w, "connection-import:next");
        assert!(wizard(&w).loading());
    }

    #[test]
    fn opening_from_a_form_replaces_the_form() {
        let mut w = ws();
        w.dispatch_connection(ConnectionAction::New);
        assert!(matches!(w.dialog(), Some(Dialog::Form(_))));
        open(&mut w);
        assert!(matches!(w.dialog(), Some(Dialog::Import(_))));
    }

    #[test]
    fn a_held_enter_from_preview_never_applies_on_the_secrets_step() {
        let mut w = ws();
        let request = open(&mut w);
        w.import_discovered(request, None, found("beekeeper"));
        run(&mut w, "connection-import:cursor-down");
        let Effect::Import(ImportEffect::ReadFile { request, .. }) =
            run(&mut w, "connection-import:next")
        else {
            panic!("expected ReadFile");
        };
        let report = sqler_import::source("beekeeper")
            .unwrap()
            .parse(BEEKEEPER.as_bytes());
        w.import_file_read(request, report);
        assert_eq!(wizard(&w).step(), ImportStep::Preview);
        // The fresh key-down moves to Secrets, where the first password field takes the focus;
        // every auto-repeat after it is dropped by the view, typing or not.
        let id = wizard(&w)
            .key_command("enter", false, false, false)
            .expect("enter is a command");
        assert!(matches!(run(&mut w, id), Effect::None));
        assert_eq!(wizard(&w).step(), ImportStep::Secrets);
        for typing in [false, true] {
            for _ in 0..5 {
                if !ImportWizard::swallows_held_key("enter", typing) {
                    run(&mut w, "connection-import:apply");
                }
            }
        }
        assert_eq!(wizard(&w).step(), ImportStep::Secrets);
        assert!(wizard(&w).summary().is_none());
    }

    #[test]
    fn the_keyboard_alone_runs_the_whole_wizard() {
        let mut w = ws();
        let request = open(&mut w);
        w.import_discovered(request, None, found("beekeeper"));
        let press = |w: &mut Workspace, key: &str| -> Effect {
            let id = wizard(w)
                .key_command(key, false, false, false)
                .unwrap_or_else(|| panic!("{key} is not a command"));
            run(w, id)
        };
        press(&mut w, "down");
        let Effect::Import(ImportEffect::ReadFile { request, .. }) = press(&mut w, "enter") else {
            panic!("expected ReadFile");
        };
        let report = sqler_import::source("beekeeper")
            .unwrap()
            .parse(BEEKEEPER.as_bytes());
        w.import_file_read(request, report);
        assert_eq!(wizard(&w).step(), ImportStep::Preview);
        press(&mut w, "down");
        press(&mut w, "space");
        assert_eq!(wizard(&w).selected_count(), 2);
        press(&mut w, "enter");
        assert_eq!(wizard(&w).step(), ImportStep::Secrets);
        press(&mut w, "backspace");
        assert_eq!(wizard(&w).step(), ImportStep::Preview);
        press(&mut w, "enter");
        let effect = press(&mut w, "enter");
        assert!(matches!(effect, Effect::SaveConnections { .. }));
        assert_eq!(wizard(&w).step(), ImportStep::Summary);
        press(&mut w, "enter");
        assert!(w.import_wizard().is_none());
        assert_eq!(w.sidebar().store().connections().len(), 2);
    }

    #[test]
    fn effect_debug_never_shows_a_path() {
        let effect = Effect::Import(ImportEffect::ReadFile {
            request: 3,
            source: "pgadmin",
            path: PathBuf::from("/home/someone/servers.json"),
        });
        let text = format!("{effect:?}");
        assert!(text.contains("pgadmin"));
        assert!(!text.contains("someone"));
    }

    #[test]
    fn ssh_key_passphrases_get_their_own_field() {
        // A key-file tunnel entry from a hand-made report.
        let mut spec = ConnectionSpec::new("Tunnelled", driver("postgres"), "postgres://h/d");
        spec = spec.with_ssh(SshProfile::new(
            "bastion",
            "me",
            SshAuth::KeyFile {
                path: PathBuf::from("/k"),
                passphrase_in_keychain: false,
            },
        ));
        let report = SourceReport {
            source: "pgadmin",
            entries: vec![sqler_import::SourceEntry {
                index: 0,
                name: "Tunnelled".to_owned(),
                group: vec![],
                outcome: sqler_import::EntryOutcome::Mapped(spec),
                unsupported: vec![],
                notes: vec![],
                secrets: vec![ProfileSecretKind::SshPassphrase],
            }],
            notices: vec![],
        };
        let mut w = ws();
        let request = open(&mut w);
        w.import_discovered(request, None, found("pgadmin"));
        let Effect::Import(ImportEffect::PickFile { request }) =
            run(&mut w, "connection-import:next")
        else {
            panic!("expected PickFile");
        };
        let Effect::Import(ImportEffect::ReadFile { request, .. }) =
            w.import_file_chosen(request, Some(PathBuf::from("/s.json")))
        else {
            panic!("expected ReadFile");
        };
        w.import_file_read(request, Ok(report));
        run(&mut w, "connection-import:next");
        let rows = wizard(&w).secret_rows();
        assert_eq!(rows[0].label, "SSH key passphrase");
        w.dispatch_connection(ConnectionAction::Import(ImportAction::SetSecret {
            index: rows[0].index,
            kind: rows[0].kind,
            text: SecretText::new("pass".to_owned()),
        }));
        let Effect::SaveConnections { secrets } = run(&mut w, "connection-import:apply") else {
            panic!("expected SaveConnections");
        };
        let [SecretOp::Set(r, _)] = secrets.as_slice() else {
            panic!("expected one Set");
        };
        assert_eq!(r.kind(), SecretKind::SshKeyPassphrase);
    }
}
