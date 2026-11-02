// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The update check as the workspace sees it (ADR-0066): a pure, headless [`UpdateModel`], the
//! [`UpdateService`] the binary implements and the commands.
//!
//! Plain Rust, no `GPUI`. The setting `check_for_updates` is off by default and this model never
//! asks the service for a check while it is off, not even for the manual command: it shows a hint
//! instead. The service is armed only after the first frame. Only a version and a release-notes
//! link, plain download states and strings ever reach the UI; the download and the install run in
//! the service (ADR-0078).

#[cfg(any(test, feature = "screenshots"))]
pub mod fake;

use std::sync::Arc;

use tokio::sync::mpsc::UnboundedReceiver;

use crate::command::{Command, CommandError, CommandId, CommandRegistry};

/// What the binary provides for update checks.
///
/// Every method returns at once; the work runs elsewhere and reports through
/// [`UpdateEvent`]s.
pub trait UpdateService: Send + Sync {
    /// Whether this build can verify a manifest (a trusted key is compiled in).
    fn configured(&self) -> bool;
    /// The first frame was drawn: arm the schedule, only when `enabled`.
    fn start(&self, enabled: bool);
    /// The setting changed. Off aborts a check in flight and stops the timer; on arms the timer.
    fn set_enabled(&self, enabled: bool);
    /// A manual check. The service ignores it while disabled.
    fn check_now(&self);
    /// Remembers that the user dismissed the notice for this version.
    fn dismiss(&self, version: &str);
    /// Installs the verified download of this version and restarts. The service reports through
    /// [`DownloadEvent`]s.
    fn install(&self, _version: &str) {}
}

/// What `run` takes from the binary for the update check.
pub struct UpdateStartup {
    /// The service; `None` in a build without update checks.
    pub service: Option<Arc<dyn UpdateService>>,
    /// The events the service sends.
    pub events: Option<UnboundedReceiver<UpdateEvent>>,
    /// The version dismissed in an earlier session.
    pub dismissed: Option<String>,
}

impl UpdateStartup {
    /// A build without update checks.
    #[must_use]
    pub fn unavailable() -> Self {
        Self {
            service: None,
            events: None,
            dismissed: None,
        }
    }
}

impl std::fmt::Debug for UpdateStartup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpdateStartup")
            .field("service", &self.service.is_some())
            .field("events", &self.events.is_some())
            .field("dismissed", &self.dismissed)
            .finish()
    }
}

/// A report from the service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateEvent {
    /// A check began.
    Started {
        /// Whether the user asked for it.
        manual: bool,
    },
    /// A check ended.
    Finished {
        /// Whether the user asked for it.
        manual: bool,
        /// How it ended.
        outcome: UpdateOutcome,
    },
    /// The download or install of an announced version moved on.
    Download(DownloadEvent),
}

/// A report about the download or install of one version. Every text is fixed and plain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadEvent {
    /// Bytes arrived.
    Progress {
        /// The version.
        version: String,
        /// Bytes received.
        received: u64,
        /// Bytes expected.
        total: u64,
    },
    /// The file passed the size and checksum checks.
    Verified {
        /// The version.
        version: String,
    },
    /// The file was not downloaded or was refused.
    Refused {
        /// The version.
        version: String,
        /// Why.
        reason: String,
    },
    /// The install began.
    Installing {
        /// The version.
        version: String,
    },
    /// `SQLer` cannot replace itself here; the verified file is at `path`.
    Manual {
        /// The version.
        version: String,
        /// Why.
        reason: String,
        /// Where the verified file is.
        path: String,
    },
    /// The install failed; the old version is intact.
    InstallFailed {
        /// The version.
        version: String,
        /// Why.
        reason: String,
    },
    /// The new version is in place.
    Restarting {
        /// The version.
        version: String,
        /// Whether the service started the new version after this one exits.
        relaunch: bool,
    },
}

/// Where the download of the announced version stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadStatus {
    /// Nothing (yet).
    None,
    /// Bytes are arriving.
    Downloading {
        /// Bytes received.
        received: u64,
        /// Bytes expected.
        total: u64,
    },
    /// Downloaded and verified; ready to install.
    Verified,
    /// Not downloaded, with the reason.
    Refused(String),
    /// Installing.
    Installing,
    /// Cannot self-install; the file is at `path`.
    Manual {
        /// Why.
        reason: String,
        /// The verified file.
        path: String,
    },
    /// The install failed, with the reason.
    InstallFailed(String),
    /// Installed; restarting.
    Restarting,
}

/// How a check ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateOutcome {
    /// This build is the newest.
    UpToDate {
        /// The newest version.
        latest: String,
    },
    /// A newer version exists.
    Available {
        /// The newer version.
        version: String,
        /// Where its release notes are.
        notes_url: String,
    },
    /// The check failed; the text is fixed and carries no URL.
    Failed {
        /// Why.
        message: String,
    },
    /// This build has no trusted key.
    NotConfigured,
}

/// An update command, each mapped to a command ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateCommand {
    /// Check now.
    CheckNow,
    /// Turn the automatic checks on or off.
    SetEnabled(bool),
    /// Open the release notes of the announced version.
    OpenNotes,
    /// Dismiss the notice.
    Dismiss,
    /// Install the verified update and restart.
    Install,
}

/// Work for the view to carry out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateEffect {
    /// Call [`UpdateService::start`].
    Start(bool),
    /// Call [`UpdateService::set_enabled`].
    SetEnabled(bool),
    /// Call [`UpdateService::check_now`].
    CheckNow,
    /// Call [`UpdateService::dismiss`].
    Dismiss(String),
    /// Open this `https` URL in the browser.
    OpenUrl(String),
    /// Call [`UpdateService::install`].
    Install(String),
    /// Quit the application (the new version is in place).
    Quit,
}

/// Whether this build can check for updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    /// No update support in this build.
    Unavailable,
    /// Update support without a trusted key.
    NotConfigured,
    /// Checks can run.
    Ready,
}

/// The newer version the status bar announces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateNotice {
    /// The newer version.
    pub version: String,
    /// Where its release notes are.
    pub notes_url: String,
}

/// The commands: ID and title.
const COMMANDS: &[(&str, &str)] = &[
    ("app:check-for-updates", "Check for Updates"),
    ("app:open-release-notes", "Open Update Release Notes"),
    ("app:dismiss-update-notice", "Dismiss Update Notice"),
    (
        "settings:toggle-update-checks",
        "Toggle Automatic Update Checks",
    ),
    ("app:install-update", "Install Update and Restart"),
];

/// Registers the update commands.
///
/// # Errors
///
/// [`CommandError::Duplicate`] when one of the IDs is already registered.
pub fn register_commands(registry: &mut CommandRegistry) -> Result<(), CommandError> {
    for (id, title) in COMMANDS {
        registry.register(Command::new(id, *title)?)?;
    }
    Ok(())
}

/// The update command behind a command ID, if it is one. `enabled` is the current setting
/// (the toggle gives the opposite).
#[must_use]
pub fn action_for(id: &CommandId, enabled: bool) -> Option<UpdateCommand> {
    match id.as_str() {
        "app:check-for-updates" => Some(UpdateCommand::CheckNow),
        "app:open-release-notes" => Some(UpdateCommand::OpenNotes),
        "app:dismiss-update-notice" => Some(UpdateCommand::Dismiss),
        "settings:toggle-update-checks" => Some(UpdateCommand::SetEnabled(!enabled)),
        "app:install-update" => Some(UpdateCommand::Install),
        _ => None,
    }
}

impl UpdateCommand {
    /// The ID of the command.
    ///
    /// # Panics
    ///
    /// Never in practice: the IDs are constants checked by a unit test.
    #[must_use]
    #[allow(clippy::expect_used)] // constant IDs, validated by the `ids_are_valid` test
    pub fn command_id(self) -> CommandId {
        let index = match self {
            Self::CheckNow => 0,
            Self::OpenNotes => 1,
            Self::Dismiss => 2,
            Self::SetEnabled(_) => 3,
            Self::Install => 4,
        };
        CommandId::new(COMMANDS[index].0).expect("constant command ID is valid")
    }
}

const TOAST_NOT_IN_BUILD: &str = "Update checks are not part of this build.";
const TOAST_NOT_CONFIGURED: &str = "Update checks are not configured in this build.";
const TOAST_NO_VERIFIED: &str = "No verified update is ready to install.";
const TOAST_OFF: &str = "Update checks are off. Turn them on in Settings > Updates.";

/// The last finished check, for the status line.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Last {
    Never,
    UpToDate(String),
    Available(String),
    Failed(String),
}

/// The state of the update check.
#[derive(Debug)]
pub struct UpdateModel {
    availability: Availability,
    checking: bool,
    notice: Option<UpdateNotice>,
    last: Last,
    dismissed: Option<String>,
    download: DownloadStatus,
    /// The newest announced version, kept across a failed re-check.
    announced: Option<String>,
}

impl UpdateModel {
    /// Builds the model from what the binary handed over. Nothing runs.
    #[must_use]
    pub fn new(startup: &UpdateStartup) -> Self {
        let availability = match &startup.service {
            None => Availability::Unavailable,
            Some(service) if service.configured() => Availability::Ready,
            Some(_) => Availability::NotConfigured,
        };
        Self {
            availability,
            checking: false,
            notice: None,
            last: Last::Never,
            dismissed: startup.dismissed.clone(),
            download: DownloadStatus::None,
            announced: None,
        }
    }

    /// Whether this build can check.
    #[must_use]
    pub fn availability(&self) -> Availability {
        self.availability
    }

    /// Whether a check has ended since the start (its result is in the status line).
    #[must_use]
    pub fn finished(&self) -> bool {
        !self.checking && self.last != Last::Never
    }

    /// Whether a check is running.
    #[must_use]
    pub fn checking(&self) -> bool {
        self.checking
    }

    /// The announced newer version, if any and not dismissed.
    #[must_use]
    pub fn notice(&self) -> Option<&UpdateNotice> {
        self.notice.as_ref()
    }

    /// Where the download of the announced version stands.
    #[must_use]
    pub fn download(&self) -> &DownloadStatus {
        &self.download
    }

    /// Whether a verified file is ready to install (also after a failed install, for a retry).
    #[must_use]
    pub fn can_install(&self) -> bool {
        matches!(
            self.download,
            DownloadStatus::Verified | DownloadStatus::InstallFailed(_)
        )
    }

    /// The announced version the download belongs to.
    fn announced(&self) -> Option<&str> {
        self.announced.as_deref()
    }

    /// One line about the last check or, once a download started, about the update.
    #[must_use]
    pub fn status_line(&self) -> String {
        if self.checking {
            return "Checking\u{2026}".to_owned();
        }
        if let (Some(v), Last::Available(_)) = (self.announced(), &self.last) {
            match &self.download {
                DownloadStatus::None | DownloadStatus::Restarting => {}
                DownloadStatus::Downloading { received, total } => {
                    return format!(
                        "Downloading SQLer {v} ({} of {} MB)",
                        received / 1_000_000,
                        total / 1_000_000
                    );
                }
                DownloadStatus::Verified => return format!("SQLer {v} is ready to install"),
                DownloadStatus::Refused(r) => {
                    return format!("SQLer {v} was not downloaded: {r}");
                }
                DownloadStatus::Installing => return format!("Installing SQLer {v}\u{2026}"),
                DownloadStatus::Manual { reason, .. } => {
                    return format!("Install SQLer {v} by hand: {reason}");
                }
                DownloadStatus::InstallFailed(r) => {
                    return format!("Could not install SQLer {v}: {r}");
                }
            }
        }
        self.check_line()
    }

    /// One line about the last check only (the Settings Status row); the download has its own row.
    #[must_use]
    pub fn check_line(&self) -> String {
        if self.checking {
            return "Checking\u{2026}".to_owned();
        }
        match &self.last {
            Last::Never => "Not checked yet".to_owned(),
            Last::UpToDate(v) => format!("Up to date (latest {v})"),
            Last::Available(v) => format!("SQLer {v} is available"),
            Last::Failed(m) => format!("Last check failed: {m}"),
        }
    }

    /// The first frame was drawn: arm the service. Only a build that can check does anything.
    pub fn first_frame(&mut self, enabled: bool) -> Option<UpdateEffect> {
        (self.availability == Availability::Ready).then_some(UpdateEffect::Start(enabled))
    }

    /// Runs a command. `enabled` is the setting as it is now (for [`UpdateCommand::SetEnabled`]
    /// it is the value before the change). Returns the work and a toast text.
    pub fn command(
        &mut self,
        command: UpdateCommand,
        enabled: bool,
    ) -> (Vec<UpdateEffect>, Option<String>) {
        match command {
            UpdateCommand::CheckNow => match self.availability {
                Availability::Unavailable => (Vec::new(), Some(TOAST_NOT_IN_BUILD.to_owned())),
                Availability::NotConfigured => (Vec::new(), Some(TOAST_NOT_CONFIGURED.to_owned())),
                Availability::Ready if !enabled => (Vec::new(), Some(TOAST_OFF.to_owned())),
                Availability::Ready if self.checking => (Vec::new(), None),
                Availability::Ready => {
                    self.checking = true;
                    (vec![UpdateEffect::CheckNow], None)
                }
            },
            UpdateCommand::Install => match self.availability {
                Availability::Unavailable => (Vec::new(), Some(TOAST_NOT_IN_BUILD.to_owned())),
                Availability::NotConfigured => (Vec::new(), Some(TOAST_NOT_CONFIGURED.to_owned())),
                Availability::Ready if !self.can_install() => {
                    (Vec::new(), Some(TOAST_NO_VERIFIED.to_owned()))
                }
                Availability::Ready => match self.announced().map(str::to_owned) {
                    Some(v) => {
                        self.download = DownloadStatus::Installing;
                        (vec![UpdateEffect::Install(v)], None)
                    }
                    None => (Vec::new(), Some(TOAST_NO_VERIFIED.to_owned())),
                },
            },
            UpdateCommand::SetEnabled(on) => {
                if self.availability != Availability::Ready {
                    return (Vec::new(), None);
                }
                if !on {
                    self.download = DownloadStatus::None;
                    self.announced = None;
                    // The service aborts the check; its late result must not linger.
                    self.checking = false;
                }
                (vec![UpdateEffect::SetEnabled(on)], None)
            }
            UpdateCommand::OpenNotes => {
                let url = self
                    .notice
                    .as_ref()
                    .map(|n| n.notes_url.as_str())
                    .filter(|u| u.starts_with("https://"));
                (
                    url.map(|u| UpdateEffect::OpenUrl(u.to_owned()))
                        .into_iter()
                        .collect(),
                    None,
                )
            }
            UpdateCommand::Dismiss => match self.notice.take() {
                Some(notice) => {
                    self.dismissed = Some(notice.version.clone());
                    (vec![UpdateEffect::Dismiss(notice.version)], None)
                }
                None => (Vec::new(), None),
            },
        }
    }

    /// Takes a report of the service. Returns a toast text; only a manual check gets one.
    pub fn event(&mut self, event: UpdateEvent) -> Option<String> {
        self.event_with_effects(event).1
    }

    /// Like [`Self::event`], but also returns the work for the view (the quit after an install).
    pub fn event_with_effects(
        &mut self,
        event: UpdateEvent,
    ) -> (Vec<UpdateEffect>, Option<String>) {
        if let UpdateEvent::Download(download) = event {
            return self.download_event(download);
        }
        (Vec::new(), self.check_event(event))
    }

    fn download_event(&mut self, event: DownloadEvent) -> (Vec<UpdateEffect>, Option<String>) {
        use DownloadEvent as E;
        let version = match &event {
            E::Progress { version, .. }
            | E::Verified { version }
            | E::Refused { version, .. }
            | E::Installing { version }
            | E::Manual { version, .. }
            | E::InstallFailed { version, .. }
            | E::Restarting { version, .. } => version.clone(),
        };
        if self.announced() != Some(version.as_str()) {
            return (Vec::new(), None);
        }
        match event {
            E::Progress {
                received, total, ..
            } => {
                self.download = DownloadStatus::Downloading { received, total };
                (Vec::new(), None)
            }
            E::Verified { .. } => {
                self.download = DownloadStatus::Verified;
                (Vec::new(), None)
            }
            E::Refused { reason, .. } => {
                self.download = DownloadStatus::Refused(reason);
                (Vec::new(), None)
            }
            E::Installing { .. } => {
                self.download = DownloadStatus::Installing;
                (Vec::new(), None)
            }
            E::Manual { reason, path, .. } => {
                let toast = format!("{reason} Verified file: {path}");
                self.download = DownloadStatus::Manual { reason, path };
                (Vec::new(), Some(toast))
            }
            E::InstallFailed { reason, .. } => {
                let toast = format!("Could not install SQLer {version}: {reason}");
                self.download = DownloadStatus::InstallFailed(reason);
                (Vec::new(), Some(toast))
            }
            E::Restarting { relaunch, .. } => {
                self.download = DownloadStatus::Restarting;
                if relaunch {
                    (vec![UpdateEffect::Quit], None)
                } else {
                    (
                        Vec::new(),
                        Some(format!(
                            "SQLer {version} is installed. Quit and reopen SQLer to use it."
                        )),
                    )
                }
            }
        }
    }

    fn check_event(&mut self, event: UpdateEvent) -> Option<String> {
        match event {
            UpdateEvent::Download(_) => None,
            UpdateEvent::Started { .. } => {
                self.checking = true;
                None
            }
            UpdateEvent::Finished { manual, outcome } => {
                self.checking = false;
                let toast = match outcome {
                    UpdateOutcome::UpToDate { latest } => {
                        self.notice = None;
                        self.download = DownloadStatus::None;
                        self.announced = None;
                        self.last = Last::UpToDate(latest);
                        "SQLer is up to date.".to_owned()
                    }
                    UpdateOutcome::Available { version, notes_url } => {
                        self.download = DownloadStatus::None;
                        self.announced = Some(version.clone());
                        let hidden = !manual && self.dismissed.as_deref() == Some(&version);
                        self.last = Last::Available(version.clone());
                        let toast = format!(
                            "SQLer {version} is available. Open the release notes from the status bar."
                        );
                        self.notice = (!hidden).then_some(UpdateNotice { version, notes_url });
                        toast
                    }
                    UpdateOutcome::Failed { message } => {
                        let toast = format!("Could not check for updates: {message}");
                        self.last = Last::Failed(message);
                        toast
                    }
                    UpdateOutcome::NotConfigured => {
                        self.availability = Availability::NotConfigured;
                        TOAST_NOT_CONFIGURED.to_owned()
                    }
                };
                // An automatic check that finds a notice shows the status item, not a toast.
                manual.then_some(toast)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::ScriptedUpdates;
    use super::*;

    fn ready() -> UpdateModel {
        let (_, startup) = ScriptedUpdates::startup(
            true,
            UpdateOutcome::UpToDate {
                latest: "1.0.0".into(),
            },
        );
        UpdateModel::new(&startup)
    }

    fn model_for(configured: bool) -> UpdateModel {
        let (_, startup) = ScriptedUpdates::startup(
            configured,
            UpdateOutcome::UpToDate {
                latest: "1.0.0".into(),
            },
        );
        UpdateModel::new(&startup)
    }

    fn available(v: &str) -> UpdateOutcome {
        UpdateOutcome::Available {
            version: v.into(),
            notes_url: format!("https://github.com/alwatroos/sqler/releases/tag/v{v}"),
        }
    }

    fn finished(m: &mut UpdateModel, manual: bool, outcome: UpdateOutcome) -> Option<String> {
        m.event(UpdateEvent::Finished { manual, outcome })
    }

    fn dl(m: &mut UpdateModel, e: DownloadEvent) -> (Vec<UpdateEffect>, Option<String>) {
        m.event_with_effects(UpdateEvent::Download(e))
    }

    fn up_to_date() -> UpdateOutcome {
        UpdateOutcome::UpToDate {
            latest: "1.0.0".into(),
        }
    }

    fn verified_model() -> UpdateModel {
        let mut m = ready();
        finished(&mut m, false, available("1.1.0"));
        dl(
            &mut m,
            DownloadEvent::Verified {
                version: "1.1.0".into(),
            },
        );
        m
    }

    #[test]
    fn download_progress_and_status_lines() {
        let mut m = ready();
        finished(&mut m, false, available("1.1.0"));
        dl(
            &mut m,
            DownloadEvent::Progress {
                version: "1.1.0".into(),
                received: 3_000_000,
                total: 12_000_000,
            },
        );
        assert_eq!(m.status_line(), "Downloading SQLer 1.1.0 (3 of 12 MB)");
        assert!(!m.can_install());
        dl(
            &mut m,
            DownloadEvent::Refused {
                version: "1.1.0".into(),
                reason: "no".into(),
            },
        );
        assert_eq!(m.status_line(), "SQLer 1.1.0 was not downloaded: no");
        dl(
            &mut m,
            DownloadEvent::Verified {
                version: "1.1.0".into(),
            },
        );
        assert_eq!(m.status_line(), "SQLer 1.1.0 is ready to install");
        assert!(m.can_install());
    }

    #[test]
    fn events_for_another_version_are_ignored() {
        let mut m = ready();
        finished(&mut m, false, available("1.1.0"));
        dl(
            &mut m,
            DownloadEvent::Verified {
                version: "1.0.5".into(),
            },
        );
        assert_eq!(*m.download(), DownloadStatus::None);
        assert!(!m.can_install());
    }

    #[test]
    fn install_is_gated_and_asks_the_service() {
        let mut m = ready();
        assert_eq!(
            m.command(UpdateCommand::Install, true),
            (Vec::new(), Some(TOAST_NO_VERIFIED.to_owned()))
        );
        let mut m = verified_model();
        assert_eq!(
            m.command(UpdateCommand::Install, true),
            (vec![UpdateEffect::Install("1.1.0".into())], None)
        );
        assert_eq!(*m.download(), DownloadStatus::Installing);
        assert_eq!(m.status_line(), "Installing SQLer 1.1.0\u{2026}");
        assert_eq!(
            m.command(UpdateCommand::Install, true).1.as_deref(),
            Some(TOAST_NO_VERIFIED)
        );
        let mut m = UpdateModel::new(&UpdateStartup::unavailable());
        assert_eq!(
            m.command(UpdateCommand::Install, true).1.as_deref(),
            Some(TOAST_NOT_IN_BUILD)
        );
        assert_eq!(
            model_for(false)
                .command(UpdateCommand::Install, true)
                .1
                .as_deref(),
            Some(TOAST_NOT_CONFIGURED)
        );
    }

    #[test]
    fn manual_failed_and_restarting_toasts() {
        let mut m = verified_model();
        let (_, toast) = dl(
            &mut m,
            DownloadEvent::Manual {
                version: "1.1.0".into(),
                reason: "By hand.".into(),
                path: "/x/y".into(),
            },
        );
        assert_eq!(toast.as_deref(), Some("By hand. Verified file: /x/y"));
        assert_eq!(m.status_line(), "Install SQLer 1.1.0 by hand: By hand.");
        let (_, toast) = dl(
            &mut m,
            DownloadEvent::InstallFailed {
                version: "1.1.0".into(),
                reason: "swap".into(),
            },
        );
        assert_eq!(
            toast.as_deref(),
            Some("Could not install SQLer 1.1.0: swap")
        );
        assert_eq!(m.status_line(), "Could not install SQLer 1.1.0: swap");
        assert!(m.can_install());
        let (effects, toast) = dl(
            &mut m,
            DownloadEvent::Restarting {
                version: "1.1.0".into(),
                relaunch: true,
            },
        );
        assert_eq!((effects, toast), (vec![UpdateEffect::Quit], None));
        let (effects, toast) = dl(
            &mut m,
            DownloadEvent::Restarting {
                version: "1.1.0".into(),
                relaunch: false,
            },
        );
        assert_eq!(effects, Vec::new());
        assert_eq!(
            toast.as_deref(),
            Some("SQLer 1.1.0 is installed. Quit and reopen SQLer to use it.")
        );
    }

    #[test]
    fn a_new_check_or_turning_off_resets_the_download() {
        let mut m = verified_model();
        finished(&mut m, false, available("1.2.0"));
        assert_eq!(*m.download(), DownloadStatus::None);
        let mut m = verified_model();
        finished(&mut m, false, up_to_date());
        assert_eq!(*m.download(), DownloadStatus::None);
        let mut m = verified_model();
        m.command(UpdateCommand::SetEnabled(false), true);
        assert_eq!(*m.download(), DownloadStatus::None);
        // A failed re-check keeps the verified file.
        let mut m = verified_model();
        finished(
            &mut m,
            false,
            UpdateOutcome::Failed {
                message: "x".into(),
            },
        );
        assert!(m.can_install());
    }

    #[test]
    fn install_command_is_registered_at_index_four() {
        assert_eq!(
            UpdateCommand::Install.command_id().as_str(),
            "app:install-update"
        );
        assert_eq!(
            action_for(&UpdateCommand::Install.command_id(), true),
            Some(UpdateCommand::Install)
        );
    }

    #[test]
    fn ids_are_valid_and_round_trip() {
        let mut registry = CommandRegistry::new();
        register_commands(&mut registry).unwrap();
        for (id, title) in COMMANDS {
            let id = CommandId::new(id).unwrap();
            assert_eq!(registry.get(&id).unwrap().title(), *title);
        }
        for cmd in [
            UpdateCommand::CheckNow,
            UpdateCommand::OpenNotes,
            UpdateCommand::Dismiss,
            UpdateCommand::Install,
        ] {
            assert_eq!(action_for(&cmd.command_id(), true), Some(cmd));
        }
        let toggle = UpdateCommand::SetEnabled(true).command_id();
        assert_eq!(
            action_for(&toggle, false),
            Some(UpdateCommand::SetEnabled(true))
        );
        assert_eq!(
            action_for(&toggle, true),
            Some(UpdateCommand::SetEnabled(false))
        );
        assert_eq!(action_for(&CommandId::new("app:quit").unwrap(), true), None);
        let mut twice = registry;
        assert!(register_commands(&mut twice).is_err());
    }

    #[test]
    fn unavailable_and_unconfigured_give_toasts_and_no_effects() {
        let mut m = UpdateModel::new(&UpdateStartup::unavailable());
        assert_eq!(m.availability(), Availability::Unavailable);
        assert_eq!(m.first_frame(true), None);
        assert_eq!(
            m.command(UpdateCommand::CheckNow, true),
            (Vec::new(), Some(TOAST_NOT_IN_BUILD.to_owned()))
        );
        assert_eq!(
            m.command(UpdateCommand::SetEnabled(true), false),
            (Vec::new(), None)
        );
        let mut m = model_for(false);
        assert_eq!(m.availability(), Availability::NotConfigured);
        assert_eq!(m.first_frame(true), None);
        assert_eq!(
            m.command(UpdateCommand::CheckNow, true),
            (Vec::new(), Some(TOAST_NOT_CONFIGURED.to_owned()))
        );
    }

    #[test]
    fn a_not_configured_outcome_switches_the_build_to_not_configured() {
        let mut m = ready();
        assert_eq!(
            finished(&mut m, true, UpdateOutcome::NotConfigured),
            Some(TOAST_NOT_CONFIGURED.to_owned())
        );
        assert_eq!(m.availability(), Availability::NotConfigured);
        assert_eq!(m.command(UpdateCommand::CheckNow, true).0, Vec::new());
        assert_eq!(finished(&mut m, false, UpdateOutcome::NotConfigured), None);
    }

    #[test]
    fn check_now_while_off_gives_a_hint_and_no_effect() {
        let mut m = ready();
        assert_eq!(
            m.command(UpdateCommand::CheckNow, false),
            (Vec::new(), Some(TOAST_OFF.to_owned()))
        );
        assert!(!m.checking());
    }

    #[test]
    fn check_now_while_checking_does_nothing() {
        let mut m = ready();
        assert_eq!(
            m.command(UpdateCommand::CheckNow, true),
            (vec![UpdateEffect::CheckNow], None)
        );
        assert!(m.checking());
        assert_eq!(m.command(UpdateCommand::CheckNow, true), (Vec::new(), None));
        assert_eq!(m.status_line(), "Checking\u{2026}");
    }

    #[test]
    fn first_frame_starts_only_a_ready_build() {
        assert_eq!(ready().first_frame(false), Some(UpdateEffect::Start(false)));
        assert_eq!(ready().first_frame(true), Some(UpdateEffect::Start(true)));
    }

    #[test]
    fn set_enabled_off_stops_the_running_check() {
        let mut m = ready();
        m.event(UpdateEvent::Started { manual: false });
        assert!(m.checking());
        assert_eq!(
            m.command(UpdateCommand::SetEnabled(false), true),
            (vec![UpdateEffect::SetEnabled(false)], None)
        );
        assert!(!m.checking());
    }

    #[test]
    fn available_sets_the_notice_and_manual_checks_toast() {
        let mut m = ready();
        assert_eq!(finished(&mut m, false, available("1.1.0")), None);
        let notice = m.notice().unwrap();
        assert_eq!(notice.version, "1.1.0");
        assert_eq!(m.status_line(), "SQLer 1.1.0 is available");
        let mut m = ready();
        assert_eq!(
            finished(&mut m, true, available("1.1.0")),
            Some("SQLer 1.1.0 is available. Open the release notes from the status bar.".into())
        );
        assert!(m.notice().is_some());
    }

    #[test]
    fn dismissed_version_is_hidden_from_automatic_checks_only() {
        let (_, mut startup) = ScriptedUpdates::startup(true, available("1.1.0"));
        startup.dismissed = Some("1.1.0".into());
        let mut m = UpdateModel::new(&startup);
        assert_eq!(finished(&mut m, false, available("1.1.0")), None);
        assert!(m.notice().is_none());
        // A newer version than the dismissed one shows.
        finished(&mut m, false, available("1.2.0"));
        assert_eq!(m.notice().unwrap().version, "1.2.0");
        // A manual check shows the dismissed version again.
        let mut m = UpdateModel::new(&startup);
        finished(&mut m, true, available("1.1.0"));
        assert_eq!(m.notice().unwrap().version, "1.1.0");
    }

    #[test]
    fn up_to_date_and_failures() {
        let mut m = ready();
        assert_eq!(m.status_line(), "Not checked yet");
        finished(&mut m, false, available("1.1.0"));
        assert_eq!(
            finished(
                &mut m,
                true,
                UpdateOutcome::UpToDate {
                    latest: "1.0.0".into()
                }
            ),
            Some("SQLer is up to date.".into())
        );
        assert!(m.notice().is_none());
        assert_eq!(m.status_line(), "Up to date (latest 1.0.0)");
        let failed = || UpdateOutcome::Failed {
            message: "the server did not answer".into(),
        };
        // Automatic failures are quiet: no toast, only the status line.
        assert_eq!(finished(&mut m, false, failed()), None);
        assert_eq!(
            m.status_line(),
            "Last check failed: the server did not answer"
        );
        assert_eq!(
            finished(&mut m, true, failed()),
            Some("Could not check for updates: the server did not answer".into())
        );
    }

    #[test]
    fn dismiss_clears_the_notice_and_names_the_version() {
        let mut m = ready();
        assert_eq!(m.command(UpdateCommand::Dismiss, true), (Vec::new(), None));
        finished(&mut m, false, available("1.1.0"));
        assert_eq!(
            m.command(UpdateCommand::Dismiss, true),
            (vec![UpdateEffect::Dismiss("1.1.0".into())], None)
        );
        assert!(m.notice().is_none());
        // The same version found again by an automatic check stays hidden.
        finished(&mut m, false, available("1.1.0"));
        assert!(m.notice().is_none());
    }

    #[test]
    fn open_notes_needs_an_https_url() {
        let mut m = ready();
        assert_eq!(m.command(UpdateCommand::OpenNotes, true).0, Vec::new());
        finished(&mut m, false, available("1.1.0"));
        assert_eq!(
            m.command(UpdateCommand::OpenNotes, true).0,
            vec![UpdateEffect::OpenUrl(
                "https://github.com/alwatroos/sqler/releases/tag/v1.1.0".into()
            )]
        );
        finished(
            &mut m,
            false,
            UpdateOutcome::Available {
                version: "1.2.0".into(),
                notes_url: "http://example.org/x".into(),
            },
        );
        assert_eq!(m.command(UpdateCommand::OpenNotes, true).0, Vec::new());
        finished(
            &mut m,
            false,
            UpdateOutcome::Available {
                version: "1.3.0".into(),
                notes_url: "javascript:alert(1)".into(),
            },
        );
        assert_eq!(m.command(UpdateCommand::OpenNotes, true).0, Vec::new());
    }
}
