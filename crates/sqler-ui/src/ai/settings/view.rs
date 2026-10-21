// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The AI section of the settings page: providers, the provider editor, models per use case,
//! redaction and limits. Colours, spacing and sizes come from the theme tokens and the shared
//! widgets; every click becomes an [`AiAction`]. A typed API key lives in a masked input and is
//! never drawn.

use gpui::{
    AnyElement, ClickEvent, Context, Div, ElementId, FontWeight, IntoElement, ParentElement as _,
    SharedString, Styled as _, div, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::Input;
use gpui_component::{ActiveTheme as _, Sizable as _};
use sqler_config::{DEFAULT_REDACTED_COLUMNS, ProviderConfig};

use crate::ai::action::AiAction;
use crate::ai::settings::{AiField, AiTestStatus, DraftError, ProviderDraft, needs_key};
use crate::ai::{AiState, ModelRow, kind_label, model_rows};
use crate::theme::SPACE;
use crate::workspace::WorkspaceView;
use crate::workspace::chrome::{field_row, ui_scale};

/// Font weight of the AI section heading (the page title uses the same).
const HEADING_WEIGHT: f32 = 600.;

/// A small button that runs an AI action: primary when `chosen`, outline otherwise.
fn btn(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    chosen: bool,
    action: AiAction,
    cx: &mut Context<WorkspaceView>,
) -> Button {
    let button = Button::new(id).label(label.into()).small();
    let button = if chosen {
        button.primary()
    } else {
        button.outline()
    };
    button.on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
        view.dispatch_ai(action.clone(), cx);
    }))
}

fn row() -> Div {
    div()
        .flex()
        .flex_row()
        .flex_wrap()
        .items_center()
        .gap(px(SPACE[2]))
}

fn muted_line(text: impl Into<SharedString>, cx: &Context<WorkspaceView>) -> Div {
    div()
        .text_size(px(ui_scale(cx).sm))
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
}

fn error_line(error: &DraftError, cx: &Context<WorkspaceView>) -> Div {
    div()
        .text_size(px(ui_scale(cx).sm))
        .text_color(cx.theme().danger)
        .child(error.message.clone())
}

/// The error of `error` when it belongs to `field`.
fn error_at(error: Option<&DraftError>, field: AiField) -> Option<&DraftError> {
    error.filter(|e| e.field == Some(field))
}

/// A text input of the section, with the error that belongs to it.
fn input_row(
    label: &'static str,
    field: AiField,
    error: Option<&DraftError>,
    view: &WorkspaceView,
    cx: &mut Context<WorkspaceView>,
) -> Option<Div> {
    let input = view.ai_input(field)?;
    let mut content = div()
        .flex()
        .flex_col()
        .gap(px(SPACE[0]))
        .child(Input::new(input));
    if let Some(error) = error_at(error, field) {
        content = content.child(error_line(error, cx));
    }
    Some(field_row(label, content, cx))
}

/// The AI section: providers, models, redaction and limits.
pub(crate) fn ai_section(view: &WorkspaceView, cx: &mut Context<WorkspaceView>) -> AnyElement {
    let ai = view.workspace().ai();
    let (border, scale) = (cx.theme().border, ui_scale(cx));
    let mut section = div()
        .w_full()
        .flex()
        .flex_col()
        .gap(px(SPACE[4]))
        .pt(px(SPACE[4]))
        .border_t_1()
        .border_color(border)
        .child(
            div()
                .text_size(px(scale.lg))
                .font_weight(FontWeight(HEADING_WEIGHT))
                .child("AI"),
        );
    if let Some(message) = ai.load_error() {
        section = section.child(div().text_color(cx.theme().danger).child(format!(
            "ai.toml could not be read: {message}. Changes are off."
        )));
    } else if !ai.persistent() && view.workspace().settings().persistent() {
        // The page already says it when its own settings are session-only too.
        section = section.child(muted_line("AI changes apply to this session only.", cx));
    }
    section
        .child(providers(ai, cx))
        .children(editor(ai, view, cx))
        .child(models(ai, cx))
        .child(limits(ai, view, cx))
        .into_any_element()
}

fn subheading(text: &'static str, cx: &Context<WorkspaceView>) -> Div {
    div()
        .text_size(px(ui_scale(cx).xs))
        .text_color(cx.theme().muted_foreground)
        .child(text)
}

/// The provider list and the kinds a new provider can be.
fn providers(ai: &AiState, cx: &mut Context<WorkspaceView>) -> Div {
    let (border, radius) = (cx.theme().border, cx.theme().radius);
    let mut list = div().flex().flex_col().gap(px(SPACE[1]));
    if ai.config().providers.is_empty() {
        list = list.child(muted_line("No providers yet. Add one below.", cx));
    }
    for (i, provider) in ai.config().providers.iter().enumerate() {
        let id = provider.id.clone();
        let detail = provider_detail(provider);
        list = list.child(
            row()
                .justify_between()
                .px(px(SPACE[3]))
                .py(px(SPACE[1]))
                .border_1()
                .border_color(border)
                .rounded(radius)
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .child(div().child(format!(
                            "{} ({})",
                            provider.id.as_str(),
                            kind_label(provider.kind)
                        )))
                        .child(muted_line(detail, cx)),
                )
                .child(
                    row()
                        .child(btn(
                            ("ai-edit", i),
                            "Edit",
                            false,
                            AiAction::EditProvider(id.clone()),
                            cx,
                        ))
                        .child(btn(
                            ("ai-remove", i),
                            "Remove",
                            false,
                            AiAction::RemoveProvider(id),
                            cx,
                        )),
                ),
        );
    }
    let mut kinds = row();
    for (i, kind) in ai.kinds().iter().enumerate() {
        kinds = kinds.child(btn(
            ("ai-add", i),
            kind_label(*kind),
            false,
            AiAction::AddProvider(*kind),
            cx,
        ));
    }
    let mut block = div()
        .flex()
        .flex_col()
        .gap(px(SPACE[2]))
        .child(subheading("PROVIDERS", cx))
        .child(list);
    if ai.kinds().is_empty() {
        block = block.child(muted_line("No provider is available in this build.", cx));
    } else {
        block = block.child(muted_line("Add provider", cx)).child(kinds);
    }
    block
}

fn provider_detail(provider: &ProviderConfig) -> String {
    let models = if provider.models.is_empty() {
        "no models listed".to_owned()
    } else {
        provider.models.join(", ")
    };
    let key = if provider.api_key_in_keychain {
        ", key in the keychain"
    } else {
        ""
    };
    format!("{models}{key}")
}

/// The provider editor card; `None` while no editor is open.
fn editor(ai: &AiState, view: &WorkspaceView, cx: &mut Context<WorkspaceView>) -> Option<Div> {
    let draft = ai.draft()?;
    let (border, radius) = (cx.theme().border, cx.theme().radius);
    let title = if draft.is_editing() {
        "Edit provider"
    } else {
        "New provider"
    };
    let mut card = div()
        .flex()
        .flex_col()
        .gap(px(SPACE[3]))
        .p(px(SPACE[4]))
        .border_1()
        .border_color(border)
        .rounded(radius)
        .child(subheading(title, cx));

    if draft.is_editing() {
        card = card.child(field_row("Id", div().child(draft.id().to_owned()), cx));
    } else {
        card = card.children(input_row(
            "Id",
            AiField::ProviderId,
            draft.error(),
            view,
            cx,
        ));
    }

    let mut kinds = row();
    if draft.is_editing() {
        kinds = kinds.child(div().child(kind_label(draft.kind())));
    } else {
        for (i, kind) in ai.kinds().iter().enumerate() {
            kinds = kinds.child(btn(
                ("ai-kind", i),
                kind_label(*kind),
                *kind == draft.kind(),
                AiAction::SetKind(*kind),
                cx,
            ));
        }
    }
    card = card.child(field_row("Kind", kinds, cx));
    card = card.children(input_row(
        "Endpoint",
        AiField::Endpoint,
        draft.error(),
        view,
        cx,
    ));
    card = card.children(input_row(
        "Models",
        AiField::Models,
        draft.error(),
        view,
        cx,
    ));
    if needs_key(draft.kind()) || draft.key_kept() {
        card = card.child(key_row(draft, view, cx));
    }
    card = card.child(field_row("", test_row(ai, draft, cx), cx));
    if let Some(error) = draft.error().filter(|e| e.field.is_none()) {
        card = card.child(error_line(error, cx));
    }
    Some(
        card.child(
            row()
                .justify_end()
                .child(btn(
                    "ai-cancel",
                    "Cancel",
                    false,
                    AiAction::CancelProvider,
                    cx,
                ))
                .child(btn("ai-save", "Save", true, AiAction::SaveProvider, cx)),
        ),
    )
}

fn key_row(draft: &ProviderDraft, view: &WorkspaceView, cx: &mut Context<WorkspaceView>) -> Div {
    let mut content = div().flex().flex_col().gap(px(SPACE[1]));
    if draft.key_kept() {
        content = content.child(
            row()
                .child(muted_line("Stored in the keychain", cx))
                .child(btn(
                    "ai-key-remove",
                    "Remove key",
                    false,
                    AiAction::ClearKey,
                    cx,
                )),
        );
    }
    if let Some(input) = view.ai_input(AiField::Key) {
        content = content.child(Input::new(input));
    }
    if let Some(error) = error_at(draft.error(), AiField::Key) {
        content = content.child(error_line(error, cx));
    }
    field_row("API key", content, cx)
}

fn test_row(ai: &AiState, draft: &ProviderDraft, cx: &mut Context<WorkspaceView>) -> Div {
    let _ = draft;
    let theme = cx.theme();
    let (success, danger) = (theme.success, theme.danger);
    let mut line = row().child(btn(
        "ai-test",
        "Test connection",
        false,
        AiAction::TestProvider,
        cx,
    ));
    match ai.test() {
        AiTestStatus::Idle => {}
        AiTestStatus::Running(_) => line = line.child(muted_line("Testing\u{2026}", cx)),
        AiTestStatus::Ok(models) => {
            let noun = if models.len() == 1 { "model" } else { "models" };
            line = line.child(
                div()
                    .text_size(px(ui_scale(cx).sm))
                    .text_color(success)
                    .child(format!("Connected: {} {noun}", models.len())),
            );
            if !models.is_empty() {
                line = line.child(btn(
                    "ai-use-models",
                    "Use listed models",
                    false,
                    AiAction::UseListedModels,
                    cx,
                ));
            }
        }
        AiTestStatus::Failed(message) => {
            line = line.child(
                div()
                    .text_size(px(ui_scale(cx).sm))
                    .text_color(danger)
                    .child(format!("Failed: {message}")),
            );
        }
    }
    line
}

/// The default model and one override per use case.
fn models(ai: &AiState, cx: &mut Context<WorkspaceView>) -> Div {
    let mut block = div()
        .flex()
        .flex_col()
        .gap(px(SPACE[2]))
        .child(subheading("MODELS", cx));
    let rows = model_rows(ai.config());
    if rows.is_empty() {
        return block.child(muted_line(
            "Add a provider with at least one model to choose models.",
            cx,
        ));
    }
    for (index, entry) in rows.into_iter().enumerate() {
        block = block.child(model_row(entry, index, cx));
    }
    block
}

fn model_row(entry: ModelRow, index: usize, cx: &mut Context<WorkspaceView>) -> Div {
    let use_case = entry.use_case;
    let mut chips = row().child(btn(
        ("ai-model-none", index),
        entry.none_label,
        entry.current.is_none(),
        AiAction::SetModel {
            use_case,
            choice: None,
        },
        cx,
    ));
    for (i, choice) in entry.choices.into_iter().enumerate() {
        chips = chips.child(btn(
            ("ai-model", index * 1000 + i),
            format!("{}/{}", choice.provider.as_str(), choice.model),
            entry.current.as_ref() == Some(&choice),
            AiAction::SetModel {
                use_case,
                choice: Some(choice),
            },
            cx,
        ));
    }
    field_row(entry.title, chips, cx)
}

/// Redaction and the query caps.
fn limits(ai: &AiState, view: &WorkspaceView, cx: &mut Context<WorkspaceView>) -> Div {
    let draft = ai.limits();
    let unlimited = draft.unlimited();
    let mut block = div()
        .flex()
        .flex_col()
        .gap(px(SPACE[3]))
        .child(subheading("REDACTION", cx))
        .child(field_row(
            "Built in",
            muted_line(DEFAULT_REDACTED_COLUMNS.join(", "), cx),
            cx,
        ))
        .children(input_row(
            "Extra",
            AiField::Redaction,
            draft.error(),
            view,
            cx,
        ))
        .child(subheading("LIMITS", cx))
        .children(input_row(
            "Max rows",
            AiField::MaxRows,
            draft.error(),
            view,
            cx,
        ))
        .children(input_row(
            "Timeout (s)",
            AiField::TimeoutSecs,
            draft.error(),
            view,
            cx,
        ))
        .child(field_row(
            "Lift limits (never on prod)",
            row()
                .child(btn(
                    "ai-limits-off",
                    "Off",
                    !unlimited,
                    AiAction::SetUnlimited(false),
                    cx,
                ))
                .child(btn(
                    "ai-limits-on",
                    "On",
                    unlimited,
                    AiAction::SetUnlimited(true),
                    cx,
                )),
            cx,
        ));
    if let Some(error) = draft.error().filter(|e| e.field.is_none()) {
        block = block.child(error_line(error, cx));
    }
    block.child(row().child(btn(
        "ai-limits-save",
        "Save",
        true,
        AiAction::SaveLimits,
        cx,
    )))
}
