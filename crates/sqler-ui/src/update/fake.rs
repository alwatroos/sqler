// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! A scripted update service for tests and screenshots: it answers a manual check with a fixed
//! outcome and records every call, so a test can prove that a call was never made.

use std::sync::{Arc, Mutex};

use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

use super::{DownloadEvent, UpdateEvent, UpdateOutcome, UpdateService, UpdateStartup};

/// The calls a [`ScriptedUpdates`] received.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Calls {
    /// The `enabled` argument of every `start`.
    pub starts: Vec<bool>,
    /// The argument of every `set_enabled`.
    pub set_enabled: Vec<bool>,
    /// How many times `check_now` was called.
    pub check_now: usize,
    /// The version of every `dismiss`.
    pub dismissed: Vec<String>,
    /// The version of every `install`.
    pub installs: Vec<String>,
}

/// An [`UpdateService`] that answers `check_now` with `Started` and then `Finished`.
#[derive(Debug)]
pub struct ScriptedUpdates {
    configured: bool,
    outcome: UpdateOutcome,
    events: UnboundedSender<UpdateEvent>,
    calls: Mutex<Calls>,
    download: Mutex<Vec<DownloadEvent>>,
}

impl ScriptedUpdates {
    /// The service and the startup to hand to the workspace.
    #[must_use]
    pub fn startup(configured: bool, outcome: UpdateOutcome) -> (Arc<Self>, UpdateStartup) {
        let (events, inbox) = unbounded_channel();
        let service = Arc::new(Self {
            configured,
            outcome,
            events,
            calls: Mutex::new(Calls::default()),
            download: Mutex::new(Vec::new()),
        });
        let startup = UpdateStartup {
            service: Some(Arc::clone(&service) as Arc<dyn UpdateService>),
            events: Some(inbox),
            dismissed: None,
        };
        (service, startup)
    }

    /// Download events sent after `Finished` of every check. Returns the service for chaining.
    #[must_use]
    pub fn with_download(self: &Arc<Self>, events: Vec<DownloadEvent>) -> Arc<Self> {
        match self.download.lock() {
            Ok(mut guard) => *guard = events,
            Err(poisoned) => *poisoned.into_inner() = events,
        }
        Arc::clone(self)
    }

    /// A copy of the recorded calls.
    #[must_use]
    pub fn calls(&self) -> Calls {
        self.with_calls(|c| c.clone())
    }

    fn with_calls<T>(&self, f: impl FnOnce(&mut Calls) -> T) -> T {
        match self.calls.lock() {
            Ok(mut guard) => f(&mut guard),
            Err(poisoned) => f(&mut poisoned.into_inner()),
        }
    }
}

impl UpdateService for ScriptedUpdates {
    fn configured(&self) -> bool {
        self.configured
    }

    fn start(&self, enabled: bool) {
        self.with_calls(|c| c.starts.push(enabled));
    }

    fn set_enabled(&self, enabled: bool) {
        self.with_calls(|c| c.set_enabled.push(enabled));
    }

    fn check_now(&self) {
        self.with_calls(|c| c.check_now += 1);
        let _ = self.events.send(UpdateEvent::Started { manual: true });
        let _ = self.events.send(UpdateEvent::Finished {
            manual: true,
            outcome: self.outcome.clone(),
        });
        let events = match self.download.lock() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        };
        for event in events {
            let _ = self.events.send(UpdateEvent::Download(event));
        }
    }

    fn dismiss(&self, version: &str) {
        self.with_calls(|c| c.dismissed.push(version.to_owned()));
    }

    fn install(&self, version: &str) {
        self.with_calls(|c| c.installs.push(version.to_owned()));
    }
}
