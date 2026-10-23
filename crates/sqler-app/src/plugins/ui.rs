// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The UI contributions as the app serves them: the sink the host pushes through, and the typed calls mapped to the
//! one-line errors the window shows. [`AppPlugins`](super::backend::AppPlugins) forwards its four `PluginsBackend`
//! methods here.

use sqler_driver_api::BoxFuture;
use sqler_plugin_api::{CommandContext, ContributionId, PluginId, WidgetEvent, WidgetTree};
use sqler_plugin_host::{PluginHost, UiSink};
use sqler_ui::plugins::backend::{PluginUiEvent, UiSender};

use super::backend::one_line;

/// What a call before the first refresh is told.
pub(super) const NOT_LOADED: &str = "plugins are not loaded yet";

/// Forwards what a plugin pushes to the window's inbox. A window that is gone drops the event.
pub(super) struct AppUiSink(pub(super) UiSender);

impl UiSink for AppUiSink {
    fn refresh_panel(&self, plugin: &PluginId, panel: &ContributionId) {
        let _ = self.0.send(PluginUiEvent::RefreshPanel {
            plugin: plugin.clone(),
            panel: panel.clone(),
        });
    }

    fn set_status(&self, plugin: &PluginId, item: &ContributionId, text: String) {
        let _ = self.0.send(PluginUiEvent::SetStatus {
            plugin: plugin.clone(),
            item: item.clone(),
            text,
        });
    }

    fn notify(&self, plugin: &PluginId, message: String) {
        let _ = self.0.send(PluginUiEvent::Notify {
            plugin: plugin.clone(),
            message,
        });
    }
}

fn refused<T: Send + 'static>() -> BoxFuture<'static, Result<T, String>> {
    Box::pin(std::future::ready(Err(NOT_LOADED.to_owned())))
}

/// `run_command` on the host, or a refusal when no host exists yet.
pub(super) fn run_command(
    host: Option<PluginHost>,
    id: &PluginId,
    command: &ContributionId,
    context: CommandContext,
) -> BoxFuture<'static, Result<(), String>> {
    let Some(host) = host else {
        return refused();
    };
    let pending = host.run_command(id, command, context);
    Box::pin(async move { pending.await.map_err(|e| one_line(&e.to_string())) })
}

/// `render_panel` on the host. A bad tree is an `Err` with the validation reason.
pub(super) fn render_panel(
    host: Option<PluginHost>,
    id: &PluginId,
    panel: &ContributionId,
) -> BoxFuture<'static, Result<WidgetTree, String>> {
    let Some(host) = host else {
        return refused();
    };
    let pending = host.render_panel(id, panel);
    Box::pin(async move {
        match pending.await {
            Ok(Ok(tree)) => Ok(tree),
            Ok(Err(bad)) => Err(one_line(&format!("the plugin sent a bad tree: {bad}"))),
            Err(e) => Err(one_line(&e.to_string())),
        }
    })
}

/// `panel_event` on the host: `Ok(None)` leaves the panel as it is.
pub(super) fn panel_event(
    host: Option<PluginHost>,
    id: &PluginId,
    panel: &ContributionId,
    event: WidgetEvent,
) -> BoxFuture<'static, Result<Option<WidgetTree>, String>> {
    let Some(host) = host else {
        return refused();
    };
    let pending = host.panel_event(id, panel, event);
    Box::pin(async move {
        match pending.await {
            Ok(None) => Ok(None),
            Ok(Some(Ok(tree))) => Ok(Some(tree)),
            Ok(Some(Err(bad))) => Err(one_line(&format!("the plugin sent a bad tree: {bad}"))),
            Err(e) => Err(one_line(&e.to_string())),
        }
    })
}

/// `status` on the host: `None` hides the item.
pub(super) fn status(
    host: Option<PluginHost>,
    id: &PluginId,
    item: &ContributionId,
) -> BoxFuture<'static, Result<Option<String>, String>> {
    let Some(host) = host else {
        return refused();
    };
    let pending = host.status(id, item);
    Box::pin(async move { pending.await.map_err(|e| one_line(&e.to_string())) })
}

#[cfg(test)]
mod tests {
    use sqler_ui::plugins::backend::ui_channel;

    use super::*;

    fn pid() -> PluginId {
        "demo".parse().unwrap()
    }

    fn cid(s: &str) -> ContributionId {
        s.parse().unwrap()
    }

    #[test]
    fn the_sink_forwards_every_request_to_the_inbox() {
        let (tx, inbox) = ui_channel();
        let mut rx = inbox.take().unwrap();
        let sink = AppUiSink(tx);
        sink.refresh_panel(&pid(), &cid("main"));
        sink.set_status(&pid(), &cid("env"), "ready".to_owned());
        sink.notify(&pid(), "hello".to_owned());
        assert_eq!(
            rx.try_recv().unwrap(),
            PluginUiEvent::RefreshPanel {
                plugin: pid(),
                panel: cid("main")
            }
        );
        assert_eq!(
            rx.try_recv().unwrap(),
            PluginUiEvent::SetStatus {
                plugin: pid(),
                item: cid("env"),
                text: "ready".to_owned()
            }
        );
        assert_eq!(
            rx.try_recv().unwrap(),
            PluginUiEvent::Notify {
                plugin: pid(),
                message: "hello".to_owned()
            }
        );
    }

    #[test]
    fn a_window_that_is_gone_does_not_panic_the_sink() {
        let (tx, inbox) = ui_channel();
        drop(inbox.take());
        AppUiSink(tx).notify(&pid(), "nobody listens".to_owned());
    }

    #[tokio::test]
    async fn calls_before_the_host_exists_are_refused() {
        let p = pid();
        let c = cid("x");
        assert_eq!(
            run_command(None, &p, &c, CommandContext::None).await,
            Err(NOT_LOADED.to_owned())
        );
        assert_eq!(render_panel(None, &p, &c).await.unwrap_err(), NOT_LOADED);
        assert_eq!(
            panel_event(None, &p, &c, WidgetEvent::Clicked("a".to_owned()))
                .await
                .unwrap_err(),
            NOT_LOADED
        );
        assert_eq!(status(None, &p, &c).await, Err(NOT_LOADED.to_owned()));
    }
}
