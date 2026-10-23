// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The import wizard (ADR-0067): a headless state machine over `sqler-import`.
//!
//! Steps: Source (pick a tool's file), Preview (choose what to import), Secrets (type passwords,
//! all optional) and Summary. Nothing is written before the user confirms on Secrets; blocking
//! work (finding files, the open-file prompt, reading and parsing) is requested as an
//! [`ImportEffect`] and answered through the `Workspace::import_*` methods. A typed secret lives in
//! a [`Secret`] only, whose `Debug` is redacted, and is never logged or put in a notice.

mod apply;
#[cfg(any(test, feature = "screenshots"))]
pub(crate) mod fixtures;
pub mod load;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use sqler_config::{ConnectionStore, ProfileSecretKind, Secret};
use sqler_driver_api::sqler_core::DriverId;
use sqler_import::{ImportError, ImportOptions, ImportPlan, PlanAction, SkipReason};

/// Shown while the default locations are searched.
pub const SCANNING_TEXT: &str = "Looking for files...";
/// Shown for a source whose file is not in the usual places.
pub const NOT_FOUND_TEXT: &str = "Not found in the usual places. Choose the file.";
/// Shown when a file parses but holds no connection.
pub const EMPTY_TEXT: &str = "The file holds no connections.";
/// Shown above the password fields.
pub const SECRETS_HINT: &str =
    "Leave a field empty to skip it; you can set the password later in the connection form.";

/// The wizard steps, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportStep {
    /// Choose the tool and its file.
    Source,
    /// Choose the connections to import.
    Preview,
    /// Type the passwords (optional).
    Secrets,
    /// What was imported.
    Summary,
}

/// A typed secret on its way to the keychain. `Debug` never shows the value.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretText(String);

impl SecretText {
    /// Wraps the typed text.
    #[must_use]
    pub fn new(text: String) -> Self {
        Self(text)
    }

    /// The secret; the text is moved, not copied.
    #[must_use]
    pub fn into_secret(self) -> Secret {
        Secret::new(self.0)
    }
}

impl std::fmt::Debug for SecretText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretText(<redacted>)")
    }
}

/// A user action on the wizard. Each maps to a command id (`ConnectionAction::command_id`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportAction {
    /// Open the wizard (searches the default locations).
    Open,
    /// Put the cursor on this source option or entry row.
    SetCursor(usize),
    /// Move the cursor up.
    CursorUp,
    /// Move the cursor down.
    CursorDown,
    /// Pick a file with the open-file prompt (for the source under the cursor).
    ChooseFile,
    /// Go to the next step (on Secrets this imports; on Summary it closes the wizard).
    Next,
    /// Go to the previous step.
    Back,
    /// Select or deselect the entry under the cursor.
    ToggleEntry,
    /// Select every entry that can be imported.
    SelectAll,
    /// Deselect every entry.
    SelectNone,
    /// Set (or, with an empty text, clear) one secret.
    SetSecret {
        /// The plan item index of the connection.
        index: usize,
        /// Which secret.
        kind: ProfileSecretKind,
        /// The typed text.
        text: SecretText,
    },
    /// Import the selected entries.
    Apply,
}

/// Blocking work the view carries out for the wizard.
#[derive(Clone, PartialEq, Eq)]
pub enum ImportEffect {
    /// Find the files at the default locations of every source, then
    /// `Workspace::import_discovered`.
    Discover {
        /// Tells this search from an older one.
        request: u64,
    },
    /// Show the open-file prompt (files only, one), then `Workspace::import_file_chosen`.
    PickFile {
        /// Tells this prompt from an older one.
        request: u64,
    },
    /// Read and parse `path` as `source` ([`load::load`]), then `Workspace::import_file_read`.
    ReadFile {
        /// Tells this read from an older one.
        request: u64,
        /// The source id.
        source: &'static str,
        /// The file.
        path: PathBuf,
    },
}

impl std::fmt::Debug for ImportEffect {
    // Never the path: it can hold the user's name.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Discover { request } => f
                .debug_struct("Discover")
                .field("request", request)
                .finish(),
            Self::PickFile { request } => f
                .debug_struct("PickFile")
                .field("request", request)
                .finish(),
            Self::ReadFile {
                request, source, ..
            } => f
                .debug_struct("ReadFile")
                .field("request", request)
                .field("source", source)
                .finish_non_exhaustive(),
        }
    }
}

/// What happens to an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryBadge {
    /// A new connection.
    Add,
    /// A new connection under another name.
    Rename,
    /// The server is already saved or repeated in the file.
    Duplicate,
    /// Not imported for another reason.
    Skipped,
}

impl EntryBadge {
    /// The badge text.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Rename => "rename",
            Self::Duplicate => "duplicate",
            Self::Skipped => "skipped",
        }
    }
}

/// One choice on the Source step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceOption {
    /// The source id.
    pub source: &'static str,
    /// The tool's name.
    pub label: &'static str,
    /// A file found at a default location; `None` when there is none.
    pub path: Option<PathBuf>,
}

/// One entry on the Preview step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryRow {
    /// The plan item index.
    pub index: usize,
    /// The name it will have (or the foreign name when it is not imported).
    pub name: String,
    /// The group path joined with ` / `; empty at the root.
    pub group: String,
    /// What happens to it.
    pub badge: EntryBadge,
    /// The driver of an added entry, the old name of a renamed one, or the reason for a skip.
    pub detail: String,
    /// Whether the entry can be toggled (only entries that will be added).
    pub selectable: bool,
    /// Whether it will be imported.
    pub selected: bool,
    /// Remarks about changed or dropped values.
    pub notes: Vec<String>,
    /// Foreign fields that are not mapped.
    pub unsupported: Vec<String>,
}

/// One password field on the Secrets step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretRow {
    /// The plan item index.
    pub index: usize,
    /// Which secret.
    pub kind: ProfileSecretKind,
    /// The connection's new name.
    pub connection: String,
    /// The field label.
    pub label: &'static str,
}

/// The outcome shown on the Summary step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportSummary {
    /// The names of the added connections.
    pub added: Vec<String>,
    /// Entries of the file that were not added.
    pub not_imported: usize,
    /// Passwords handed to the keychain.
    pub passwords_stored: usize,
    /// Passwords left out (empty fields).
    pub passwords_skipped: usize,
    /// Connections that could not be added, with the reason.
    pub failed: Vec<(String, String)>,
}

/// The fixed text of an import error. `label` is the tool's name.
#[must_use]
pub fn error_text(error: &ImportError, label: &str) -> String {
    match error {
        ImportError::Io { .. } => "The file could not be read.".to_owned(),
        ImportError::NotAFile => "The chosen path is not a file.".to_owned(),
        ImportError::TooLarge { limit } => {
            format!(
                "The file is larger than {} MiB and was not read.",
                limit / (1024 * 1024)
            )
        }
        ImportError::NotUtf8 => "The file is not UTF-8 text.".to_owned(),
        ImportError::Malformed {
            line,
            column,
            message,
        } => format!("The file is malformed at line {line}, column {column}: {message}."),
        ImportError::UnrecognisedFormat(m) => format!("This is not a {label} file: {m}."),
        ImportError::TooManyEntries { limit } => {
            format!("The file holds more than {limit} connections.")
        }
        _ => "The file could not be imported.".to_owned(),
    }
}

/// The import wizard.
#[derive(Debug)]
pub struct ImportWizard {
    step: ImportStep,
    scanning: bool,
    discovered: Vec<(&'static str, Vec<PathBuf>)>,
    home: Option<PathBuf>,
    cursor: usize,
    loading: bool,
    error: Option<String>,
    /// The request whose answer is awaited.
    pending: Option<u64>,
    /// The source the open-file prompt is for.
    pick_source: Option<&'static str>,
    /// The file being read, then the file the plan comes from.
    reading: Option<(&'static str, PathBuf)>,
    file_label: Option<String>,
    plan: Option<ImportPlan>,
    selected: BTreeSet<usize>,
    secrets: BTreeMap<(usize, ProfileSecretKind), Secret>,
    summary: Option<ImportSummary>,
}

impl ImportWizard {
    /// A wizard that waits for the search of the default locations; `request` is its id.
    pub(crate) fn new(request: u64) -> Self {
        Self {
            step: ImportStep::Source,
            scanning: true,
            discovered: Vec::new(),
            home: None,
            cursor: 0,
            loading: false,
            error: None,
            pending: Some(request),
            pick_source: None,
            reading: None,
            file_label: None,
            plan: None,
            selected: BTreeSet::new(),
            secrets: BTreeMap::new(),
            summary: None,
        }
    }

    // ---- getters ----

    /// The current step.
    #[must_use]
    pub fn step(&self) -> ImportStep {
        self.step
    }

    /// Whether the default locations are still being searched.
    #[must_use]
    pub fn scanning(&self) -> bool {
        self.scanning
    }

    /// The choices of the Source step; empty while scanning. One per file found, or one without a
    /// path for a source with none.
    #[must_use]
    pub fn options(&self) -> Vec<SourceOption> {
        if self.scanning {
            return Vec::new();
        }
        let mut options = Vec::new();
        for source in sqler_import::sources() {
            let found = self
                .discovered
                .iter()
                .find(|(id, _)| *id == source.id())
                .map(|(_, paths)| paths.as_slice())
                .unwrap_or_default();
            if found.is_empty() {
                options.push(SourceOption {
                    source: source.id(),
                    label: source.label(),
                    path: None,
                });
            }
            options.extend(found.iter().map(|p| SourceOption {
                source: source.id(),
                label: source.label(),
                path: Some(p.clone()),
            }));
        }
        options
    }

    /// The cursor: an option on Source, an entry row on Preview.
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Whether a file is being read.
    #[must_use]
    pub fn loading(&self) -> bool {
        self.loading
    }

    /// The error of the last read, shown on Source.
    #[must_use]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The tool and file the plan comes from, for the Preview header.
    #[must_use]
    pub fn file_label(&self) -> Option<String> {
        self.file_label.clone()
    }

    /// File-level remarks of the report.
    #[must_use]
    pub fn notices(&self) -> &[String] {
        self.plan.as_ref().map_or(&[], |p| p.notices.as_slice())
    }

    /// The entries of the Preview step, in file order.
    #[must_use]
    pub fn rows(&self) -> Vec<EntryRow> {
        let Some(plan) = &self.plan else {
            return Vec::new();
        };
        plan.items
            .iter()
            .map(|item| {
                let (name, badge, detail) = match &item.action {
                    PlanAction::Add {
                        spec,
                        renamed_from: None,
                    } => (
                        spec.name.clone(),
                        EntryBadge::Add,
                        spec.driver.as_str().to_owned(),
                    ),
                    PlanAction::Add {
                        spec,
                        renamed_from: Some(old),
                    } => (
                        spec.name.clone(),
                        EntryBadge::Rename,
                        format!("renamed from {old}"),
                    ),
                    PlanAction::Skip(reason) => (
                        item.foreign_name.clone(),
                        match reason {
                            SkipReason::Duplicate { .. } | SkipReason::DuplicateInFile { .. } => {
                                EntryBadge::Duplicate
                            }
                            _ => EntryBadge::Skipped,
                        },
                        reason.message(),
                    ),
                };
                let selectable = matches!(item.action, PlanAction::Add { .. });
                EntryRow {
                    index: item.index,
                    name,
                    group: item.group.join(" / "),
                    badge,
                    detail,
                    selectable,
                    selected: selectable && self.selected.contains(&item.index),
                    notes: item.notes.clone(),
                    unsupported: item.unsupported.clone(),
                }
            })
            .collect()
    }

    /// How many entries will be imported.
    #[must_use]
    pub fn selected_count(&self) -> usize {
        self.selected.len()
    }

    /// The password fields of the Secrets step: for each selected connection, the secrets its
    /// source lists.
    #[must_use]
    pub fn secret_rows(&self) -> Vec<SecretRow> {
        let Some(plan) = &self.plan else {
            return Vec::new();
        };
        let mut rows = Vec::new();
        for item in &plan.items {
            let PlanAction::Add { spec, .. } = &item.action else {
                continue;
            };
            if !self.selected.contains(&item.index) {
                continue;
            }
            for &kind in &item.secrets {
                rows.push(SecretRow {
                    index: item.index,
                    kind,
                    connection: spec.name.clone(),
                    label: secret_label(kind),
                });
            }
        }
        rows
    }

    /// Whether a non-empty secret was typed for this field.
    #[must_use]
    pub fn secret_is_set(&self, index: usize, kind: ProfileSecretKind) -> bool {
        self.secrets.contains_key(&(index, kind))
    }

    /// The outcome, once the import ran.
    #[must_use]
    pub fn summary(&self) -> Option<&ImportSummary> {
        self.summary.as_ref()
    }

    /// Whether "Next" does something now.
    #[must_use]
    pub fn can_next(&self) -> bool {
        match self.step {
            ImportStep::Source => {
                !self.scanning && !self.loading && self.cursor < self.options().len()
            }
            ImportStep::Preview => self.selected_count() > 0,
            ImportStep::Secrets | ImportStep::Summary => true,
        }
    }

    /// Whether "Back" does something now.
    #[must_use]
    pub fn can_back(&self) -> bool {
        matches!(self.step, ImportStep::Preview | ImportStep::Secrets) && !self.loading
    }

    /// A path for display: the home directory shown as `~`.
    #[must_use]
    pub fn display_path(&self, path: &Path) -> String {
        if let Some(home) = &self.home
            && let Ok(rest) = path.strip_prefix(home)
        {
            let tail: Vec<String> = rest
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            return if tail.is_empty() {
                "~".to_owned()
            } else {
                format!("~/{}", tail.join("/"))
            };
        }
        path.display().to_string()
    }

    /// The command a key runs while the wizard is open, if any. `typing` is true while a password
    /// field has the focus; then, as with any other modifier, no key is a wizard command.
    #[must_use]
    pub fn key_command(
        &self,
        key: &str,
        shift: bool,
        other_modifiers: bool,
        typing: bool,
    ) -> Option<&'static str> {
        if typing || other_modifiers || shift {
            return None;
        }
        let browsing = matches!(self.step, ImportStep::Source | ImportStep::Preview);
        match key {
            "up" if browsing => Some("connection-import:cursor-up"),
            "down" if browsing => Some("connection-import:cursor-down"),
            "space" if self.step == ImportStep::Preview => Some("connection-import:toggle-entry"),
            "enter" if self.step == ImportStep::Secrets => Some("connection-import:apply"),
            "enter" => Some("connection-import:next"),
            "backspace" if self.can_back() => Some("connection-import:back"),
            _ => None,
        }
    }

    // ---- state changes (called by Workspace) ----

    /// Whether `request` is the answer the wizard waits for.
    pub(crate) fn awaits(&self, request: u64) -> bool {
        self.pending == Some(request)
    }

    /// The search finished.
    pub(crate) fn discovered(
        &mut self,
        home: Option<PathBuf>,
        found: Vec<(&'static str, Vec<PathBuf>)>,
    ) {
        self.pending = None;
        self.scanning = false;
        self.home = home;
        self.discovered = found;
        self.cursor = 0;
    }

    /// Moves the cursor to `to`, clamped to the list of the step.
    pub(crate) fn set_cursor(&mut self, to: usize) {
        let len = match self.step {
            ImportStep::Source => self.options().len(),
            ImportStep::Preview => self.rows().len(),
            ImportStep::Secrets | ImportStep::Summary => return,
        };
        if self.step == ImportStep::Source {
            self.error = None;
        }
        self.cursor = to.min(len.saturating_sub(1));
    }

    pub(crate) fn cursor_up(&mut self) {
        self.set_cursor(self.cursor.saturating_sub(1));
    }

    pub(crate) fn cursor_down(&mut self) {
        self.set_cursor(self.cursor.saturating_add(1));
    }

    /// Toggles the entry under the cursor, if it can be imported.
    pub(crate) fn toggle_entry(&mut self) {
        if self.step != ImportStep::Preview {
            return;
        }
        let rows = self.rows();
        if let Some(row) = rows.get(self.cursor).filter(|r| r.selectable)
            && !self.selected.remove(&row.index)
        {
            self.selected.insert(row.index);
        }
    }

    pub(crate) fn select_all(&mut self) {
        if self.step == ImportStep::Preview {
            self.selected = self
                .rows()
                .iter()
                .filter(|r| r.selectable)
                .map(|r| r.index)
                .collect();
        }
    }

    pub(crate) fn select_none(&mut self) {
        if self.step == ImportStep::Preview {
            self.selected.clear();
        }
    }

    /// Stores or clears one typed secret; a field that is not on the Secrets step is ignored.
    pub(crate) fn set_secret(&mut self, index: usize, kind: ProfileSecretKind, text: SecretText) {
        if self.step != ImportStep::Secrets
            || !self
                .secret_rows()
                .iter()
                .any(|r| r.index == index && r.kind == kind)
        {
            return;
        }
        let secret = text.into_secret();
        if secret.expose().is_empty() {
            self.secrets.remove(&(index, kind));
        } else {
            self.secrets.insert((index, kind), secret);
        }
    }

    /// "Choose file" on Source: the prompt for the source under the cursor.
    pub(crate) fn choose_file(&mut self, request: u64) -> Option<ImportEffect> {
        if self.step != ImportStep::Source || self.scanning || self.loading {
            return None;
        }
        let source = self.options().get(self.cursor)?.source;
        self.pick_source = Some(source);
        self.pending = Some(request);
        Some(ImportEffect::PickFile { request })
    }

    /// The open-file prompt answered. `None` keeps the Source step as it was.
    pub(crate) fn file_chosen(
        &mut self,
        request: u64,
        path: Option<PathBuf>,
    ) -> Option<ImportEffect> {
        let source = self.pick_source.take();
        let (Some(path), Some(source)) = (path, source) else {
            self.pending = None;
            return None;
        };
        Some(self.begin_read(request, source, path))
    }

    /// "Next" on Source: read the file under the cursor, or ask for one.
    pub(crate) fn next_from_source(&mut self, request: u64) -> Option<ImportEffect> {
        if self.scanning || self.loading {
            return None;
        }
        let option = self.options().into_iter().nth(self.cursor)?;
        match option.path {
            Some(path) => Some(self.begin_read(request, option.source, path)),
            None => self.choose_file(request),
        }
    }

    pub(crate) fn begin_read(
        &mut self,
        request: u64,
        source: &'static str,
        path: PathBuf,
    ) -> ImportEffect {
        self.error = None;
        self.loading = true;
        self.pending = Some(request);
        self.reading = Some((source, path.clone()));
        ImportEffect::ReadFile {
            request,
            source,
            path,
        }
    }

    /// The read finished: plan against the live store, or show the error.
    pub(crate) fn read_done(
        &mut self,
        result: Result<sqler_import::SourceReport, ImportError>,
        store: &ConnectionStore,
        drivers: &[DriverId],
    ) {
        self.pending = None;
        self.loading = false;
        let label = self
            .reading
            .as_ref()
            .and_then(|(id, _)| sqler_import::source(id))
            .map_or("connection", |s| s.label());
        match result {
            Err(error) => self.error = Some(error_text(&error, label)),
            Ok(report) if report.entries.is_empty() => self.error = Some(EMPTY_TEXT.to_owned()),
            Ok(report) => {
                let options = ImportOptions {
                    available_drivers: drivers.to_vec(),
                };
                let plan = sqler_import::plan(&report, store, &options);
                self.selected = plan
                    .items
                    .iter()
                    .filter(|i| matches!(i.action, PlanAction::Add { .. }))
                    .map(|i| i.index)
                    .collect();
                self.file_label = self
                    .reading
                    .as_ref()
                    .map(|(_, path)| format!("{label} - {}", self.display_path(path)));
                self.plan = Some(plan);
                self.secrets.clear();
                self.error = None;
                self.cursor = 0;
                self.step = ImportStep::Preview;
            }
        }
    }

    /// "Next" on Preview.
    pub(crate) fn goto_secrets(&mut self) {
        if self.step == ImportStep::Preview && self.selected_count() > 0 {
            self.step = ImportStep::Secrets;
            self.cursor = 0;
        }
    }

    /// "Back".
    pub(crate) fn back(&mut self) {
        match self.step {
            ImportStep::Preview => {
                self.plan = None;
                self.selected.clear();
                self.file_label = None;
                self.step = ImportStep::Source;
                self.cursor = 0;
            }
            ImportStep::Secrets => {
                self.secrets.clear();
                self.step = ImportStep::Preview;
                self.cursor = 0;
            }
            ImportStep::Source | ImportStep::Summary => {}
        }
    }
}

fn secret_label(kind: ProfileSecretKind) -> &'static str {
    match kind {
        ProfileSecretKind::DbPassword => "Database password",
        ProfileSecretKind::SshPassword => "SSH password",
        ProfileSecretKind::SshPassphrase => "SSH key passphrase",
        _ => "Secret",
    }
}

#[cfg(test)]
#[allow(clippy::assert_is_empty)]
mod tests {
    use super::*;

    fn wizard_with_files() -> ImportWizard {
        let mut w = ImportWizard::new(1);
        w.discovered(
            Some(PathBuf::from("/home/test")),
            vec![
                ("pgadmin", vec![]),
                ("beekeeper", vec![PathBuf::from("/home/test/bk.json")]),
                ("azure-data-studio", vec![]),
            ],
        );
        w
    }

    #[test]
    fn secret_text_debug_is_redacted() {
        let text = SecretText::new("hunter2-value".to_owned());
        assert_eq!(format!("{text:?}"), "SecretText(<redacted>)");
        let action = ImportAction::SetSecret {
            index: 0,
            kind: ProfileSecretKind::DbPassword,
            text,
        };
        assert!(!format!("{action:?}").contains("hunter2"));
    }

    #[test]
    fn options_follow_the_sources_and_the_files_found() {
        let mut w = ImportWizard::new(1);
        assert!(w.scanning());
        assert!(w.options().is_empty());
        w.discovered(None, vec![("beekeeper", vec![PathBuf::from("a.json")])]);
        let options = w.options();
        let ids: Vec<_> = options.iter().map(|o| o.source).collect();
        assert_eq!(ids, ["pgadmin", "beekeeper", "azure-data-studio"]);
        assert_eq!(options[0].path, None);
        assert_eq!(options[1].path, Some(PathBuf::from("a.json")));
        assert_eq!(options[1].label, "Beekeeper Studio");
    }

    #[test]
    fn two_files_of_one_source_are_two_options() {
        let mut w = ImportWizard::new(1);
        w.discovered(
            None,
            vec![(
                "pgadmin",
                vec![PathBuf::from("a.json"), PathBuf::from("b.json")],
            )],
        );
        assert_eq!(w.options().len(), 4);
    }

    #[test]
    fn display_path_uses_a_tilde() {
        let w = wizard_with_files();
        assert_eq!(
            w.display_path(Path::new("/home/test/x/y.json")),
            "~/x/y.json"
        );
        assert_eq!(w.display_path(Path::new("/home/test")), "~");
        assert_eq!(
            w.display_path(Path::new("/elsewhere/y.json")),
            "/elsewhere/y.json"
        );
    }

    #[test]
    fn cursor_is_clamped_and_clears_the_error() {
        let mut w = wizard_with_files();
        w.error = Some("x".to_owned());
        w.cursor_down();
        assert_eq!(w.cursor(), 1);
        assert_eq!(w.error(), None);
        w.set_cursor(99);
        assert_eq!(w.cursor(), 2);
        w.cursor_up();
        w.cursor_up();
        w.cursor_up();
        assert_eq!(w.cursor(), 0);
    }

    #[test]
    fn error_texts_are_fixed() {
        let e = |err: ImportError| error_text(&err, "pgAdmin 4");
        assert_eq!(
            e(ImportError::Io {
                kind: std::io::ErrorKind::NotFound
            }),
            "The file could not be read."
        );
        assert_eq!(e(ImportError::NotAFile), "The chosen path is not a file.");
        assert_eq!(
            e(ImportError::TooLarge { limit: 4 << 20 }),
            "The file is larger than 4 MiB and was not read."
        );
        assert_eq!(e(ImportError::NotUtf8), "The file is not UTF-8 text.");
        assert_eq!(
            e(ImportError::Malformed {
                line: 3,
                column: 7,
                message: "unexpected token".to_owned()
            }),
            "The file is malformed at line 3, column 7: unexpected token."
        );
        assert_eq!(
            e(ImportError::UnrecognisedFormat("no servers")),
            "This is not a pgAdmin 4 file: no servers."
        );
        assert_eq!(
            e(ImportError::TooManyEntries { limit: 5000 }),
            "The file holds more than 5000 connections."
        );
    }

    #[test]
    fn badge_labels() {
        assert_eq!(EntryBadge::Add.label(), "add");
        assert_eq!(EntryBadge::Rename.label(), "rename");
        assert_eq!(EntryBadge::Duplicate.label(), "duplicate");
        assert_eq!(EntryBadge::Skipped.label(), "skipped");
    }

    fn at(step: ImportStep) -> ImportWizard {
        let mut w = wizard_with_files();
        w.step = step;
        w
    }

    #[test]
    fn key_command_table() {
        let key = |w: &ImportWizard, k: &str| w.key_command(k, false, false, false);
        let source = at(ImportStep::Source);
        assert_eq!(key(&source, "up"), Some("connection-import:cursor-up"));
        assert_eq!(key(&source, "down"), Some("connection-import:cursor-down"));
        assert_eq!(key(&source, "space"), None);
        assert_eq!(key(&source, "enter"), Some("connection-import:next"));
        assert_eq!(key(&source, "backspace"), None);
        assert_eq!(key(&source, "x"), None);

        let mut preview = at(ImportStep::Preview);
        assert_eq!(key(&preview, "up"), Some("connection-import:cursor-up"));
        assert_eq!(
            key(&preview, "space"),
            Some("connection-import:toggle-entry")
        );
        assert_eq!(key(&preview, "enter"), Some("connection-import:next"));
        assert_eq!(key(&preview, "backspace"), Some("connection-import:back"));
        preview.loading = true;
        assert_eq!(key(&preview, "backspace"), None);

        let secrets = at(ImportStep::Secrets);
        assert_eq!(key(&secrets, "up"), None);
        assert_eq!(key(&secrets, "enter"), Some("connection-import:apply"));
        assert_eq!(key(&secrets, "backspace"), Some("connection-import:back"));

        let summary = at(ImportStep::Summary);
        assert_eq!(key(&summary, "enter"), Some("connection-import:next"));
        assert_eq!(key(&summary, "backspace"), None);
        assert_eq!(key(&summary, "up"), None);

        for w in [&source, &preview, &secrets, &summary] {
            for k in ["up", "down", "space", "enter", "backspace"] {
                assert_eq!(w.key_command(k, false, false, true), None, "typing {k}");
                assert_eq!(w.key_command(k, false, true, false), None, "modifier {k}");
                assert_eq!(w.key_command(k, true, false, false), None, "shift {k}");
            }
        }
    }
}
