// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `GPUI` side of the AI settings (ADR-0050): the text inputs of the settings page and the
//! two effects. `ai.toml` is written and the provider is built on the blocking pool; the test
//! call runs on tokio (ADR-0007). Neither runs on the UI thread or at startup.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use gpui::{AppContext as _, Context, Entity, Subscription, Window};
use gpui_component::input::{InputEvent, InputState};
use sqler_config::{ConfigDir, ProviderConfig, Secret};

use super::WorkspaceView;
use crate::ai::{AiAction, AiField, AiProviderFactory};
use crate::workspace::ai::PendingSave;
use crate::workspace::{Effect, SecretOp};

/// How long a provider test waits for the model list.
const TEST_TIMEOUT: Duration = Duration::from_secs(15);

/// The text fields of the editor, in the order they are created.
const EDITOR_FIELDS: [AiField; 4] = [
    AiField::ProviderId,
    AiField::Endpoint,
    AiField::Models,
    AiField::Key,
];

/// The text fields of the limits form, in the order they are created.
const LIMIT_FIELDS: [AiField; 3] = [AiField::Redaction, AiField::MaxRows, AiField::TimeoutSecs];

/// The text inputs of the AI section of the settings page.
pub(crate) struct AiInputs {
    /// The `AiState::inputs_generation` these inputs were made for.
    generation: u64,
    fields: HashMap<AiField, Entity<InputState>>,
    _subscriptions: Vec<Subscription>,
}

fn placeholder(field: AiField) -> &'static str {
    match field {
        AiField::ProviderId => "provider id, e.g. ollama",
        AiField::Endpoint => "provider default",
        AiField::Models => "model-a, model-b",
        AiField::Key => "API key",
        AiField::Redaction => "column fragments, comma separated",
        AiField::MaxRows => "1000",
        AiField::TimeoutSecs => "30",
    }
}

impl AiInputs {
    /// The input of a field.
    pub(crate) fn field(&self, field: AiField) -> Option<&Entity<InputState>> {
        self.fields.get(&field)
    }

    /// Makes the inputs for the state on screen: the limits form always, the editor's fields
    /// while the editor is open. The key input is masked and starts empty.
    fn build(
        ai: &crate::ai::AiState,
        window: &mut Window,
        cx: &mut Context<WorkspaceView>,
    ) -> Self {
        let mut fields = HashMap::new();
        let mut subscriptions = Vec::new();
        let mut make =
            |field: AiField, value: String, masked: bool, cx: &mut Context<WorkspaceView>| {
                let input = cx.new(|cx| {
                    InputState::new(window, cx)
                        .masked(masked)
                        .placeholder(placeholder(field))
                        .default_value(value)
                });
                subscriptions.push(cx.subscribe(
                    &input,
                    move |view, input, event: &InputEvent, cx| {
                        if matches!(event, InputEvent::Change) {
                            let text = input.read(cx).value().to_string();
                            view.dispatch_ai(AiAction::SetText { field, text }, cx);
                        }
                    },
                ));
                fields.insert(field, input);
            };
        if let Some(draft) = ai.draft() {
            for field in EDITOR_FIELDS {
                let value = draft.text(field).to_owned();
                make(field, value, field == AiField::Key, cx);
            }
        }
        for field in LIMIT_FIELDS {
            let value = ai.limits().text(field).to_owned();
            make(field, value, false, cx);
        }
        Self {
            generation: ai.inputs_generation(),
            fields,
            _subscriptions: subscriptions,
        }
    }
}

impl WorkspaceView {
    /// Runs an AI action through the workspace and carries out the effect.
    pub(crate) fn dispatch_ai(&mut self, action: AiAction, cx: &mut Context<Self>) {
        let effect = self.ws.dispatch_ai(action);
        self.apply(effect, cx);
        self.settle(cx);
    }

    /// The input of a field of the AI settings, while the settings page is open. The key input is
    /// masked.
    pub(crate) fn ai_input(&self, field: AiField) -> Option<&Entity<InputState>> {
        self.ai_inputs.as_ref()?.field(field)
    }

    /// Makes the inputs match the AI settings: they exist only while the settings page is shown
    /// and are rebuilt when the editor opens or closes or a draft is reset.
    pub(super) fn sync_ai_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.ws.settings_tab_active() {
            self.ai_inputs = None;
        } else if self
            .ai_inputs
            .as_ref()
            .is_none_or(|i| i.generation != self.ws.ai().inputs_generation())
        {
            self.ai_inputs = Some(AiInputs::build(self.ws.ai(), window, cx));
        }
    }

    /// Carries out the AI effects.
    pub(super) fn apply_ai(&mut self, effect: Effect, cx: &mut Context<Self>) {
        match effect {
            Effect::SaveAiConfig {
                dir,
                config,
                secrets,
            } => self.save_ai_config(dir, config, secrets, cx),
            Effect::TestAiProvider {
                request,
                provider,
                key,
            } => self.test_ai_provider(request, provider, key, cx),
            _ => {}
        }
    }

    /// Saves `ai.toml` and then the keychain changes. Saves run one at a time: while one is in
    /// flight a newer one waits, and only the latest waiting configuration is written.
    fn save_ai_config(
        &mut self,
        dir: Option<ConfigDir>,
        config: sqler_config::AiConfig,
        secrets: Vec<SecretOp>,
        cx: &mut Context<Self>,
    ) {
        let save = PendingSave {
            dir,
            config,
            secrets,
        };
        if let Some(save) = self.ai_saves.submit(save) {
            self.run_ai_save(save, cx);
        }
    }

    /// Writes `ai.toml` on a blocking thread, then applies the keychain changes of this save;
    /// the keychain is left alone when the file could not be written. When both are done the
    /// next waiting save starts.
    fn run_ai_save(&mut self, save: PendingSave, cx: &mut Context<Self>) {
        let PendingSave {
            dir,
            config,
            secrets,
        } = save;
        let done = self.bridge.blocking(move || match dir {
            Some(dir) => dir.save_ai_config(&config).map_err(|e| e.to_string()),
            None => Ok(()),
        });
        cx.spawn(async move |this, cx| {
            let result = done
                .await
                .unwrap_or_else(|_| Err("the save task was lost".to_owned()));
            let written = this
                .update(cx, |view, cx| {
                    let keychain = result.is_ok() && !secrets.is_empty();
                    if result.is_err() && !secrets.is_empty() {
                        view.notices
                            .push("The keychain was not changed.".to_owned());
                    }
                    let written = keychain.then(|| view.bridge.write_secrets(secrets));
                    view.ws.ai_saved(result);
                    view.settle(cx);
                    written
                })
                .ok();
            let Some(written) = written else { return };
            let failures = match written {
                Some(written) => written.await.unwrap_or_default(),
                None => Vec::new(),
            };
            this.update(cx, |view, cx| {
                for failure in failures {
                    view.notices.push(format!("Keychain: {failure}"));
                }
                if let Some(next) = view.ai_saves.finish() {
                    view.run_ai_save(next, cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Builds the provider on the blocking pool (it may read the keychain) and lists its models
    /// on tokio, within [`TEST_TIMEOUT`].
    fn test_ai_provider(
        &mut self,
        request: u64,
        provider: ProviderConfig,
        key: Option<Secret>,
        cx: &mut Context<Self>,
    ) {
        let factory: Arc<dyn AiProviderFactory> = Arc::clone(&self.ai_factory);
        let done = self.bridge.spawn(async move {
            let built =
                tokio::task::spawn_blocking(move || factory.build(&provider, key.as_ref())).await;
            let provider = match built {
                Ok(Ok(provider)) => provider,
                Ok(Err(err)) => return Err(err.to_string()),
                Err(_) => return Err("the test task was lost".to_owned()),
            };
            match tokio::time::timeout(TEST_TIMEOUT, provider.list_models()).await {
                Ok(Ok(models)) => Ok(models.into_iter().map(|m| m.id).collect::<Vec<_>>()),
                Ok(Err(err)) => Err(err.to_string()),
                Err(_) => Err("The provider did not answer within 15 seconds.".to_owned()),
            }
        });
        cx.spawn(async move |this, cx| {
            let result = done
                .await
                .unwrap_or_else(|_| Err("the test was lost".to_owned()));
            this.update(cx, |view, cx| {
                view.ws.ai_test_finished(request, result);
                view.settle(cx);
            })
            .ok();
        })
        .detach();
    }
}
