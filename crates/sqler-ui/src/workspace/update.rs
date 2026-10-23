// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The update check inside the workspace (ADR-0066).
//!
//! State only, like the plugin manager: each call changes [`UpdateModel`] and returns the work as
//! [`Effect::Update`]; the view calls the service. While `check_for_updates` is off the model
//! gives no effect for a check, so the service is never asked for one.

use super::state::{Effect, Workspace};
use crate::update::{UpdateCommand, UpdateEffect, UpdateEvent, UpdateModel, UpdateStartup};

impl Workspace {
    /// Builds the update model from what the binary handed over. Nothing runs yet.
    #[must_use]
    pub fn with_update(mut self, startup: &UpdateStartup) -> Self {
        self.update = UpdateModel::new(startup);
        self
    }

    /// The update model.
    #[must_use]
    pub fn update(&self) -> &UpdateModel {
        &self.update
    }

    /// The effects of the model as one effect.
    fn update_effects(effects: Vec<UpdateEffect>) -> Effect {
        let mut effects: Vec<Effect> = effects.into_iter().map(Effect::Update).collect();
        match effects.len() {
            0 => Effect::None,
            1 => effects.remove(0),
            _ => Effect::Batch(effects),
        }
    }

    /// The first frame was drawn: arm the update service with the current setting.
    pub fn update_first_frame(&mut self) -> Effect {
        let enabled = self.settings.settings().check_for_updates;
        Self::update_effects(self.update.first_frame(enabled).into_iter().collect())
    }

    /// Runs an update command. `SetEnabled` changes and saves the setting first.
    pub fn dispatch_update(&mut self, command: UpdateCommand) -> Effect {
        let enabled = self.settings.settings().check_for_updates;
        let (effects, toast) = self.update.command(command, enabled);
        if let Some(toast) = toast {
            self.notice = Some(toast);
        }
        let work = Self::update_effects(effects);
        if let UpdateCommand::SetEnabled(on) = command
            && self.settings.set_check_for_updates(on)
        {
            return match work {
                Effect::None => Effect::ApplySettings,
                work => Effect::Batch(vec![Effect::ApplySettings, work]),
            };
        }
        work
    }

    /// Takes a report of the update service. A toast, if any, goes to the workspace notice.
    pub fn update_event(&mut self, event: UpdateEvent) -> Effect {
        if let Some(toast) = self.update.event(event) {
            self.notice = Some(toast);
        }
        Effect::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sidebar::SidebarModel;
    use crate::update::UpdateOutcome;
    use crate::update::fake::ScriptedUpdates;

    fn workspace(
        configured: bool,
        outcome: UpdateOutcome,
    ) -> (Workspace, std::sync::Arc<ScriptedUpdates>) {
        let (service, startup) = ScriptedUpdates::startup(configured, outcome);
        let ws = Workspace::new(SidebarModel::from_connections(Vec::new())).with_update(&startup);
        (ws, service)
    }

    fn up_to_date() -> UpdateOutcome {
        UpdateOutcome::UpToDate {
            latest: "1.0.0".into(),
        }
    }

    /// Carries out the update effects the way the view does: only through the service.
    fn run(effect: Effect, service: &ScriptedUpdates) {
        use crate::update::UpdateService as _;
        match effect {
            Effect::Batch(all) => all.into_iter().for_each(|e| run(e, service)),
            Effect::Update(UpdateEffect::Start(on)) => service.start(on),
            Effect::Update(UpdateEffect::SetEnabled(on)) => service.set_enabled(on),
            Effect::Update(UpdateEffect::CheckNow) => service.check_now(),
            Effect::Update(UpdateEffect::Dismiss(v)) => service.dismiss(&v),
            _ => {}
        }
    }

    #[test]
    fn setting_off_check_now_makes_no_call_and_hints() {
        let (mut ws, service) = workspace(true, up_to_date());
        assert!(!ws.settings().settings().check_for_updates);
        let effect = ws.dispatch_update(UpdateCommand::CheckNow);
        assert!(matches!(effect, Effect::None), "{effect:?}");
        run(effect, &service);
        assert_eq!(service.calls().check_now, 0);
        assert_eq!(
            ws.take_notice().as_deref(),
            Some("Update checks are off. Turn them on in Settings > Updates.")
        );
    }

    #[test]
    fn first_frame_passes_the_setting_and_arms_nothing_when_off() {
        let (mut ws, service) = workspace(true, up_to_date());
        let effect = ws.update_first_frame();
        assert!(matches!(effect, Effect::Update(UpdateEffect::Start(false))));
        run(effect, &service);
        let calls = service.calls();
        assert_eq!(calls.starts, [false]);
        assert_eq!(calls.check_now, 0);
        assert_eq!(calls.set_enabled, Vec::<bool>::new());
    }

    #[test]
    fn a_build_without_update_support_never_calls_anything() {
        let mut ws = Workspace::new(SidebarModel::from_connections(Vec::new()));
        assert!(matches!(ws.update_first_frame(), Effect::None));
        assert!(matches!(
            ws.dispatch_update(UpdateCommand::CheckNow),
            Effect::None
        ));
        assert_eq!(
            ws.take_notice().as_deref(),
            Some("Update checks are not part of this build.")
        );
    }

    #[test]
    fn turning_on_saves_the_setting_then_a_manual_check_runs_through_the_service() {
        let (mut ws, service) = workspace(true, up_to_date());
        let effect = ws.dispatch_update(UpdateCommand::SetEnabled(true));
        assert!(ws.settings().settings().check_for_updates);
        let Effect::Batch(parts) = &effect else {
            panic!("expected a batch: {effect:?}");
        };
        assert!(matches!(parts[0], Effect::ApplySettings));
        assert!(matches!(
            parts[1],
            Effect::Update(UpdateEffect::SetEnabled(true))
        ));
        run(effect, &service);
        assert_eq!(service.calls().set_enabled, [true]);
        // Toggling to the same value again changes nothing.
        let again = ws.dispatch_update(UpdateCommand::SetEnabled(true));
        assert!(!matches!(again, Effect::None));
        let effect = ws.dispatch_update(UpdateCommand::CheckNow);
        run(effect, &service);
        assert_eq!(service.calls().check_now, 1);
    }

    #[test]
    fn turning_off_again_stops_checks_and_blocks_the_manual_command() {
        let (mut ws, service) = workspace(true, up_to_date());
        run(
            ws.dispatch_update(UpdateCommand::SetEnabled(true)),
            &service,
        );
        run(
            ws.dispatch_update(UpdateCommand::SetEnabled(false)),
            &service,
        );
        assert_eq!(service.calls().set_enabled, [true, false]);
        assert!(!ws.settings().settings().check_for_updates);
        run(ws.dispatch_update(UpdateCommand::CheckNow), &service);
        assert_eq!(service.calls().check_now, 0);
    }

    #[test]
    fn events_become_the_notice_and_toasts_only_for_manual_checks() {
        let (mut ws, _service) = workspace(true, up_to_date());
        ws.update_event(UpdateEvent::Finished {
            manual: false,
            outcome: UpdateOutcome::Available {
                version: "1.1.0".into(),
                notes_url: "https://example.org/notes".into(),
            },
        });
        assert_eq!(ws.update().notice().unwrap().version, "1.1.0");
        assert_eq!(ws.take_notice(), None);
        ws.update_event(UpdateEvent::Finished {
            manual: true,
            outcome: UpdateOutcome::Failed {
                message: "offline".into(),
            },
        });
        assert_eq!(
            ws.take_notice().as_deref(),
            Some("Could not check for updates: offline")
        );
    }

    #[test]
    fn dismiss_reaches_the_service() {
        let (mut ws, service) = workspace(true, up_to_date());
        ws.update_event(UpdateEvent::Finished {
            manual: false,
            outcome: UpdateOutcome::Available {
                version: "1.1.0".into(),
                notes_url: "https://example.org/notes".into(),
            },
        });
        run(ws.dispatch_update(UpdateCommand::Dismiss), &service);
        assert_eq!(service.calls().dismissed, ["1.1.0"]);
        assert!(ws.update().notice().is_none());
    }
}
