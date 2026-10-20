// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The connection dialogs: the new/edit form, the delete confirmation, the group-name dialog,
//! the move dialog and the SSH host-key prompt, drawn as an overlay above the workspace.
//!
//! Views only render `Dialog` state. Text inputs are `InputState` entities owned by
//! [`DialogInputs`]; every change becomes a `ConnectionAction::SetText`.

use std::collections::HashMap;

use gpui::Focusable as _;
use gpui::{
    AnyElement, AppContext as _, ClickEvent, Context, Div, ElementId, Entity, Hsla,
    InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px, rgb,
};
use gpui_component::ActiveTheme as _;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::spinner::Spinner;
use sqler_config::{GroupId, TlsMode};
use sqler_driver_api::sqler_core::{DriverId, Environment};

use super::action::ConnectionAction;
use super::dialog::{Dialog, GroupPurpose};
use super::form::{
    COLOR_PRESETS, ConnectionForm, FormField, FormMode, SecretField, SecretSlot, SshAuthKind,
    TestStatus,
};
use crate::tabs::Rgb;
use crate::workspace::{Workspace, WorkspaceView};

/// The text fields of the form, in the order they are created.
const TEXT_FIELDS: [FormField; 10] = [
    FormField::Name,
    FormField::Target,
    FormField::Color,
    FormField::CaCert,
    FormField::ClientCert,
    FormField::ClientKey,
    FormField::SshHost,
    FormField::SshPort,
    FormField::SshUser,
    FormField::SshKeyPath,
];

/// The text inputs of the dialog on screen.
pub(crate) struct DialogInputs {
    generation: u64,
    fields: HashMap<FormField, Entity<InputState>>,
    group_name: Option<Entity<InputState>>,
    _subscriptions: Vec<Subscription>,
}

fn placeholder(field: FormField) -> &'static str {
    match field {
        FormField::Name => "e.g. Billing (prod)",
        FormField::Target => "postgres://host:5432/database?user=name, or a file path",
        FormField::Color => "#rrggbb",
        FormField::CaCert => "CA certificate file",
        FormField::ClientCert => "client certificate file",
        FormField::ClientKey => "client key file",
        FormField::SshHost => "bastion.example.com",
        FormField::SshPort => "22",
        FormField::SshUser => "user",
        FormField::SshKeyPath => "~/.ssh/id_ed25519",
        FormField::DbPassword => "database password",
        FormField::SshSecret => "SSH password",
    }
}

impl DialogInputs {
    /// The dialog generation these inputs were made for.
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    /// The input of a form field.
    pub(crate) fn field(&self, field: FormField) -> Option<&Entity<InputState>> {
        self.fields.get(&field)
    }

    /// Makes the inputs for the dialog on screen; `None` for dialogs without text inputs.
    pub(crate) fn build(
        ws: &Workspace,
        window: &mut Window,
        cx: &mut Context<WorkspaceView>,
    ) -> Option<Self> {
        let generation = ws.dialog_generation();
        match ws.dialog()? {
            Dialog::Form(form) => {
                let mut fields = HashMap::new();
                let mut subscriptions = Vec::new();
                for field in TEXT_FIELDS {
                    let value = form.text(field).to_owned();
                    let input = cx.new(|cx| {
                        InputState::new(window, cx)
                            .placeholder(placeholder(field))
                            .default_value(value)
                    });
                    subscriptions.push(subscribe(&input, field, cx));
                    fields.insert(field, input);
                }
                for field in [FormField::DbPassword, FormField::SshSecret] {
                    let input = cx.new(|cx| {
                        InputState::new(window, cx)
                            .masked(true)
                            .placeholder(placeholder(field))
                    });
                    subscriptions.push(subscribe(&input, field, cx));
                    fields.insert(field, input);
                }
                if let Some(name) = fields.get(&FormField::Name) {
                    window.focus(&name.focus_handle(cx), cx);
                }
                Some(Self {
                    generation,
                    fields,
                    group_name: None,
                    _subscriptions: subscriptions,
                })
            }
            Dialog::GroupName { text, .. } => {
                let value = text.clone();
                let input = cx.new(|cx| {
                    InputState::new(window, cx)
                        .placeholder("Group name")
                        .default_value(value)
                });
                let subscription =
                    cx.subscribe(&input, |view, input, event: &InputEvent, cx| match event {
                        InputEvent::Change => {
                            let text = input.read(cx).value().to_string();
                            view.dispatch_connection(ConnectionAction::SetGroupName(text), cx);
                        }
                        InputEvent::PressEnter { .. } => {
                            view.dispatch_connection(ConnectionAction::SubmitGroupName, cx);
                        }
                        _ => {}
                    });
                window.focus(&input.focus_handle(cx), cx);
                Some(Self {
                    generation,
                    fields: HashMap::new(),
                    group_name: Some(input),
                    _subscriptions: vec![subscription],
                })
            }
            _ => None,
        }
    }
}

/// Reports every change of a form input; Enter saves.
fn subscribe(
    input: &Entity<InputState>,
    field: FormField,
    cx: &mut Context<WorkspaceView>,
) -> Subscription {
    cx.subscribe(
        input,
        move |view, input, event: &InputEvent, cx| match event {
            InputEvent::Change => {
                let text = input.read(cx).value().to_string();
                view.dispatch_connection(ConnectionAction::SetText { field, text }, cx);
            }
            InputEvent::PressEnter { .. } => view.dispatch_connection(ConnectionAction::Save, cx),
            _ => {}
        },
    )
}

/// Theme colours the dialogs use, copied out of the theme.
#[derive(Clone, Copy)]
struct Colors {
    fg: Hsla,
    muted: Hsla,
    border: Hsla,
    danger: Hsla,
    danger_fg: Hsla,
    success: Hsla,
    primary: Hsla,
    primary_fg: Hsla,
}

impl Colors {
    fn of(cx: &Context<WorkspaceView>) -> Self {
        let theme = cx.theme();
        Self {
            fg: theme.popover_foreground,
            muted: theme.muted_foreground,
            border: theme.border,
            danger: theme.danger,
            danger_fg: theme.danger_foreground,
            success: theme.success,
            primary: theme.primary,
            primary_fg: theme.primary_foreground,
        }
    }
}

/// A clickable chip that dispatches a connection action.
fn chip(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    selected: bool,
    action: ConnectionAction,
    colors: Colors,
    cx: &mut Context<WorkspaceView>,
) -> gpui::Stateful<Div> {
    let mut chip = div()
        .id(id)
        .px_2()
        .py_0p5()
        .rounded_sm()
        .border_1()
        .border_color(colors.border)
        .text_sm()
        .cursor_pointer()
        .child(label.into())
        .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
            view.dispatch_connection(action.clone(), cx);
        }));
    if selected {
        chip = chip
            .bg(colors.primary)
            .text_color(colors.primary_fg)
            .border_color(colors.primary);
    }
    chip
}

/// A primary or danger button.
fn button(
    id: &'static str,
    label: &'static str,
    action: ConnectionAction,
    fill: Option<(Hsla, Hsla)>,
    colors: Colors,
    cx: &mut Context<WorkspaceView>,
) -> gpui::Stateful<Div> {
    let mut button = div()
        .id(id)
        .px_3()
        .py_1()
        .rounded_md()
        .border_1()
        .border_color(colors.border)
        .cursor_pointer()
        .child(label)
        .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
            view.dispatch_connection(action.clone(), cx);
        }));
    if let Some((bg, ink)) = fill {
        button = button.bg(bg).text_color(ink).border_color(bg);
    }
    button
}

/// A label, a body and the field's error.
fn labelled(
    label: &'static str,
    body: impl IntoElement,
    error: Option<&str>,
    colors: Colors,
) -> Div {
    let mut row = div()
        .flex()
        .flex_col()
        .gap_1()
        .child(div().text_xs().text_color(colors.muted).child(label))
        .child(body);
    if let Some(error) = error {
        row = row.child(
            div()
                .text_xs()
                .text_color(colors.danger)
                .child(error.to_owned()),
        );
    }
    row
}

fn text_row(
    label: &'static str,
    field: FormField,
    form: &ConnectionForm,
    inputs: &DialogInputs,
    colors: Colors,
) -> Option<Div> {
    let input = inputs.fields.get(&field)?;
    Some(labelled(
        label,
        Input::new(input),
        form.error_for(field),
        colors,
    ))
}

fn chips() -> Div {
    div().flex().flex_row().flex_wrap().gap_1()
}

/// Renders the dialog on screen as a full-window overlay; `None` when there is none.
pub(crate) fn dialog_overlay(
    ws: &Workspace,
    inputs: Option<&DialogInputs>,
    cx: &mut Context<WorkspaceView>,
) -> Option<AnyElement> {
    let colors = Colors::of(cx);
    let (panel_bg, panel_border) = (cx.theme().popover, cx.theme().border);
    let body = match ws.dialog()? {
        Dialog::Form(form) => form_panel(ws, form, inputs?, colors, cx).into_any_element(),
        Dialog::ConfirmDelete { name, .. } => confirm_panel(name, colors, cx).into_any_element(),
        Dialog::GroupName { purpose, error, .. } => {
            group_panel(purpose, error.as_deref(), inputs?, colors, cx).into_any_element()
        }
        Dialog::MoveTo { id } => {
            let current = ws.sidebar().profile(id).and_then(|p| p.group().cloned());
            move_panel(ws, id, current.as_ref(), colors, cx).into_any_element()
        }
        Dialog::TrustHostKey { key, .. } => host_key_panel(key, colors, cx).into_any_element(),
    };
    Some(
        div()
            .id("dialog-scrim")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui::black().opacity(0.45))
            .occlude()
            .child(
                div()
                    .id("dialog")
                    .w(px(580.))
                    .max_h(px(720.))
                    .flex()
                    .flex_col()
                    .bg(panel_bg)
                    .text_color(colors.fg)
                    .border_1()
                    .border_color(panel_border)
                    .rounded_lg()
                    .shadow_lg()
                    .child(body),
            )
            .into_any_element(),
    )
}

fn title(text: String) -> Div {
    div().px_4().pt_4().pb_2().text_lg().child(text)
}

fn footer(buttons: Vec<AnyElement>) -> Div {
    div()
        .flex()
        .flex_row()
        .justify_end()
        .gap_2()
        .p_4()
        .children(buttons)
}

#[allow(clippy::too_many_lines)] // one declarative element tree
fn form_panel(
    ws: &Workspace,
    form: &ConnectionForm,
    inputs: &DialogInputs,
    colors: Colors,
    cx: &mut Context<WorkspaceView>,
) -> Div {
    let heading = match form.mode() {
        FormMode::New => "New connection".to_owned(),
        FormMode::Edit(_) => format!("Edit connection {}", form.text(FormField::Name)),
    };

    // Driver choices: the compiled-in drivers, plus the connection's own if it is not one.
    let mut drivers: Vec<DriverId> = ws.drivers().to_vec();
    if let Some(current) = form.driver()
        && !drivers.contains(current)
    {
        drivers.push(current.clone());
    }
    let mut driver_chips = chips();
    for driver in drivers {
        let selected = form.driver() == Some(&driver);
        driver_chips = driver_chips.child(chip(
            ElementId::Name(SharedString::from(format!("driver-{driver}"))),
            driver.to_string(),
            selected,
            ConnectionAction::SetDriver(driver),
            colors,
            cx,
        ));
    }
    if ws.drivers().is_empty() && form.driver().is_none() {
        driver_chips = driver_chips.child(
            div()
                .text_sm()
                .text_color(colors.muted)
                .child("No database driver is compiled in."),
        );
    }

    let mut env_chips = chips();
    for (index, (label, env)) in [
        ("None", None),
        ("Dev", Some(Environment::Dev)),
        ("Staging", Some(Environment::Staging)),
        ("Prod", Some(Environment::Prod)),
    ]
    .into_iter()
    .enumerate()
    {
        env_chips = env_chips.child(chip(
            ("environment", index),
            label,
            form.environment() == env,
            ConnectionAction::SetEnvironment(env),
            colors,
            cx,
        ));
    }

    let mut swatches = div().flex().flex_row().flex_wrap().items_center().gap_1();
    let current_color = form.text(FormField::Color).trim().to_ascii_lowercase();
    for (index, &preset) in COLOR_PRESETS.iter().enumerate() {
        let Some(Rgb(value)) = Rgb::parse(preset) else {
            continue;
        };
        let selected = current_color == preset;
        let mut swatch = div()
            .id(("swatch", index))
            .size(px(20.))
            .rounded_full()
            .bg(rgb(value))
            .border_2()
            .border_color(if selected { colors.fg } else { colors.border })
            .cursor_pointer()
            .on_click(cx.listener(move |view, _: &ClickEvent, window, cx| {
                view.dispatch_connection(
                    ConnectionAction::SetText {
                        field: FormField::Color,
                        text: preset.to_owned(),
                    },
                    cx,
                );
                view.set_form_input(FormField::Color, preset, window, cx);
            }));
        if selected {
            swatch = swatch.border_color(colors.fg);
        }
        swatches = swatches.child(swatch);
    }

    let mut group_chips = chips().child(chip(
        "group-root",
        "Top level",
        form.group().is_none(),
        ConnectionAction::SetGroup(None),
        colors,
        cx,
    ));
    for (index, option) in ws.sidebar().group_options().into_iter().enumerate() {
        let selected = form.group() == Some(&option.id);
        group_chips = group_chips.child(chip(
            ("group-choice", index),
            option.path,
            selected,
            ConnectionAction::SetGroup(Some(option.id)),
            colors,
            cx,
        ));
    }

    let mut tls_chips = chips().child(chip(
        "tls-target",
        "From target",
        form.tls_mode().is_none(),
        ConnectionAction::SetTlsMode(None),
        colors,
        cx,
    ));
    for (index, mode) in [
        TlsMode::Disable,
        TlsMode::Prefer,
        TlsMode::Require,
        TlsMode::VerifyCa,
        TlsMode::VerifyFull,
    ]
    .into_iter()
    .enumerate()
    {
        tls_chips = tls_chips.child(chip(
            ("tls-mode", index),
            mode.as_str(),
            form.tls_mode() == Some(mode),
            ConnectionAction::SetTlsMode(Some(mode)),
            colors,
            cx,
        ));
    }

    let mut body = div()
        .id("dialog-body")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .px_4()
        .flex()
        .flex_col()
        .gap_3()
        .children(text_row("Name", FormField::Name, form, inputs, colors))
        .child(labelled("Driver", driver_chips, None, colors))
        .children(text_row("Target", FormField::Target, form, inputs, colors))
        .child(password_block(
            "Password",
            SecretSlot::Db,
            FormField::DbPassword,
            form,
            inputs,
            colors,
            cx,
        ))
        .child(labelled("Environment", env_chips, None, colors))
        .child(labelled(
            "Colour",
            swatches,
            form.error_for(FormField::Color),
            colors,
        ))
        .children(text_row(
            "Colour (#rrggbb)",
            FormField::Color,
            form,
            inputs,
            colors,
        ))
        .child(labelled("Group", group_chips, None, colors))
        .child(labelled("TLS", tls_chips, None, colors));

    if form.tls_mode().is_some() {
        body = body
            .children(text_row(
                "CA certificate",
                FormField::CaCert,
                form,
                inputs,
                colors,
            ))
            .children(text_row(
                "Client certificate",
                FormField::ClientCert,
                form,
                inputs,
                colors,
            ))
            .children(text_row(
                "Client key",
                FormField::ClientKey,
                form,
                inputs,
                colors,
            ));
    }

    let ssh_on = form.ssh_enabled();
    body = body.child(labelled(
        "SSH tunnel",
        chips()
            .child(chip(
                "ssh-off",
                "Off",
                !ssh_on,
                ConnectionAction::SetSshEnabled(false),
                colors,
                cx,
            ))
            .child(chip(
                "ssh-on",
                "On",
                ssh_on,
                ConnectionAction::SetSshEnabled(true),
                colors,
                cx,
            )),
        None,
        colors,
    ));
    if ssh_on {
        body = body
            .children(text_row(
                "SSH host",
                FormField::SshHost,
                form,
                inputs,
                colors,
            ))
            .children(text_row(
                "SSH port",
                FormField::SshPort,
                form,
                inputs,
                colors,
            ))
            .children(text_row(
                "SSH user",
                FormField::SshUser,
                form,
                inputs,
                colors,
            ));
        let mut auth = chips();
        for (index, (label, kind)) in [
            ("Password", SshAuthKind::Password),
            ("Key file", SshAuthKind::KeyFile),
            ("SSH agent", SshAuthKind::Agent),
        ]
        .into_iter()
        .enumerate()
        {
            auth = auth.child(chip(
                ("ssh-auth", index),
                label,
                form.ssh_auth() == kind,
                ConnectionAction::SetSshAuth(kind),
                colors,
                cx,
            ));
        }
        body = body.child(labelled("SSH authentication", auth, None, colors));
        match form.ssh_auth() {
            SshAuthKind::Password => {
                body = body.child(password_block(
                    "SSH password",
                    SecretSlot::Ssh,
                    FormField::SshSecret,
                    form,
                    inputs,
                    colors,
                    cx,
                ));
            }
            SshAuthKind::KeyFile => {
                body = body
                    .children(text_row(
                        "Private key file",
                        FormField::SshKeyPath,
                        form,
                        inputs,
                        colors,
                    ))
                    .child(password_block(
                        "Key passphrase (optional)",
                        SecretSlot::Ssh,
                        FormField::SshSecret,
                        form,
                        inputs,
                        colors,
                        cx,
                    ));
            }
            SshAuthKind::Agent => {}
        }
    }

    for error in form.errors().iter().filter(|e| e.field.is_none()) {
        body = body.child(
            div()
                .text_sm()
                .text_color(colors.danger)
                .child(error.message.clone()),
        );
    }
    body = body.child(test_row(form, colors, cx));

    div()
        .flex()
        .flex_col()
        .min_h_0()
        .child(title(heading))
        .child(body)
        .child(footer(vec![
            button(
                "form-cancel",
                "Cancel",
                ConnectionAction::Cancel,
                None,
                colors,
                cx,
            )
            .into_any_element(),
            button(
                "form-save",
                "Save",
                ConnectionAction::Save,
                Some((colors.primary, colors.primary_fg)),
                colors,
                cx,
            )
            .into_any_element(),
        ]))
}

/// A masked password input with what will happen to the stored secret.
fn password_block(
    label: &'static str,
    slot: SecretSlot,
    field: FormField,
    form: &ConnectionForm,
    inputs: &DialogInputs,
    colors: Colors,
    cx: &mut Context<WorkspaceView>,
) -> Div {
    let plan = match slot {
        SecretSlot::Db => form.db_password(),
        SecretSlot::Ssh => form.ssh_secret(),
    };
    let stored = form.original().is_some_and(|p| match slot {
        SecretSlot::Db => p.password_ref().is_some(),
        SecretSlot::Ssh => p.ssh_secret_ref().is_some(),
    });
    let (note, can_remove) = match plan {
        SecretField::Keep => ("Stored in the OS keychain. Leave empty to keep it.", true),
        SecretField::Set(_) => ("The new value is stored in the OS keychain on save.", false),
        SecretField::Clear if stored => ("The stored value is removed on save.", false),
        SecretField::Clear => (
            "Stored in the OS keychain, never in connections.toml.",
            false,
        ),
    };
    let mut hint = div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .child(div().text_xs().text_color(colors.muted).child(note));
    if can_remove {
        hint = hint.child(
            div()
                .id(ElementId::Name(SharedString::from(format!(
                    "remove-secret-{label}"
                ))))
                .text_xs()
                .text_color(colors.danger)
                .cursor_pointer()
                .child("Remove")
                .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                    view.dispatch_connection(ConnectionAction::ClearSecret(slot), cx);
                })),
        );
    }
    let input = inputs
        .fields
        .get(&field)
        .map(|i| Input::new(i).mask_toggle());
    labelled(label, div().children(input), form.error_for(field), colors).child(hint)
}

fn test_row(
    form: &ConnectionForm,
    colors: Colors,
    cx: &mut Context<WorkspaceView>,
) -> impl IntoElement + use<> {
    let mut row = div().flex().flex_row().items_center().gap_2().pb_2();
    match &form.test {
        TestStatus::Running(_) => {
            row = row
                .child(Spinner::new())
                .child(div().text_sm().child("Testing the connection…"))
                .child(button(
                    "test-cancel",
                    "Cancel test",
                    ConnectionAction::CancelTest,
                    None,
                    colors,
                    cx,
                ));
        }
        status => {
            row = row.child(button(
                "test-run",
                "Test connection",
                ConnectionAction::Test,
                None,
                colors,
                cx,
            ));
            match status {
                TestStatus::Ok => {
                    row = row.child(
                        div()
                            .text_sm()
                            .text_color(colors.success)
                            .child("Connected."),
                    );
                }
                TestStatus::Failed(message) => {
                    row = row.child(
                        div()
                            .flex_1()
                            .text_sm()
                            .text_color(colors.danger)
                            .child(message.clone()),
                    );
                }
                _ => {}
            }
        }
    }
    row
}

fn confirm_panel(
    name: &str,
    colors: Colors,
    cx: &mut Context<WorkspaceView>,
) -> impl IntoElement + use<> {
    div()
        .flex()
        .flex_col()
        .child(title(format!("Delete connection \u{201c}{name}\u{201d}?")))
        .child(div().px_4().text_sm().text_color(colors.muted).child(
            "The connection is removed from connections.toml and its stored passwords are removed from the OS keychain. This cannot be undone. Open tabs stay open.",
        ))
        .child(footer(vec![
            button("delete-cancel", "Cancel", ConnectionAction::Cancel, None, colors, cx)
                .into_any_element(),
            button(
                "delete-confirm",
                "Delete",
                ConnectionAction::ConfirmDelete,
                Some((colors.danger, colors.danger_fg)),
                colors,
                cx,
            )
            .into_any_element(),
        ]))
}

fn group_panel(
    purpose: &GroupPurpose,
    error: Option<&str>,
    inputs: &DialogInputs,
    colors: Colors,
    cx: &mut Context<WorkspaceView>,
) -> impl IntoElement + use<> {
    let heading = match purpose {
        GroupPurpose::New { .. } => "New group",
        GroupPurpose::Rename(_) => "Rename group",
    };
    let input = inputs.group_name.as_ref().map(Input::new);
    let mut body = div().px_4().flex().flex_col().gap_1().children(input);
    if let Some(error) = error {
        body = body.child(
            div()
                .text_xs()
                .text_color(colors.danger)
                .child(error.to_owned()),
        );
    }
    div()
        .flex()
        .flex_col()
        .child(title(heading.to_owned()))
        .child(body)
        .child(footer(vec![
            button(
                "group-cancel",
                "Cancel",
                ConnectionAction::Cancel,
                None,
                colors,
                cx,
            )
            .into_any_element(),
            button(
                "group-ok",
                "OK",
                ConnectionAction::SubmitGroupName,
                Some((colors.primary, colors.primary_fg)),
                colors,
                cx,
            )
            .into_any_element(),
        ]))
}

fn move_panel(
    ws: &Workspace,
    id: &sqler_config::ConnectionId,
    current: Option<&GroupId>,
    colors: Colors,
    cx: &mut Context<WorkspaceView>,
) -> impl IntoElement + use<> {
    let mut list = div()
        .id("move-list")
        .px_4()
        .flex()
        .flex_col()
        .gap_1()
        .max_h(px(360.))
        .overflow_y_scroll();
    list = list.child(chip(
        "move-root",
        "Top level",
        current.is_none(),
        ConnectionAction::MoveConnection {
            id: id.clone(),
            to: None,
            index: usize::MAX,
        },
        colors,
        cx,
    ));
    for (index, option) in ws.sidebar().group_options().into_iter().enumerate() {
        let selected = current == Some(&option.id);
        list = list.child(
            div()
                .ml(px(crate::sidebar::view::indent(option.depth)))
                .child(chip(
                    ("move-group", index),
                    option.path,
                    selected,
                    ConnectionAction::MoveConnection {
                        id: id.clone(),
                        to: Some(option.id),
                        index: usize::MAX,
                    },
                    colors,
                    cx,
                )),
        );
    }
    div()
        .flex()
        .flex_col()
        .child(title("Move connection to group".to_owned()))
        .child(list)
        .child(footer(vec![
            button(
                "move-cancel",
                "Cancel",
                ConnectionAction::Cancel,
                None,
                colors,
                cx,
            )
            .into_any_element(),
        ]))
}

fn host_key_panel(
    key: &super::connector::HostKeyPrompt,
    colors: Colors,
    cx: &mut Context<WorkspaceView>,
) -> impl IntoElement + use<> {
    let line = |label: &'static str, value: String| {
        div()
            .flex()
            .flex_row()
            .gap_2()
            .child(div().w(px(90.)).text_color(colors.muted).child(label))
            .child(div().flex_1().min_w_0().child(value))
    };
    div()
        .flex()
        .flex_col()
        .child(title("Trust this SSH host key?".to_owned()))
        .child(
            div()
                .px_4()
                .flex()
                .flex_col()
                .gap_1()
                .text_sm()
                .child(line("Server", format!("{}:{}", key.host, key.port)))
                .child(line("Algorithm", key.algorithm.clone()))
                .child(line("Fingerprint", key.fingerprint.clone()))
                .child(div().pt_2().text_xs().text_color(colors.muted).child(
                    "This server is not in known_hosts yet. Trust it only if the fingerprint matches the one the server administrator gave you. Trusting adds it to known_hosts and tries again.",
                )),
        )
        .child(footer(vec![
            button(
                "hostkey-reject",
                "Cancel",
                ConnectionAction::RejectHostKey,
                None,
                colors,
                cx,
            )
            .into_any_element(),
            button(
                "hostkey-trust",
                "Trust and continue",
                ConnectionAction::TrustHostKey,
                Some((colors.primary, colors.primary_fg)),
                colors,
                cx,
            )
            .into_any_element(),
        ]))
}
