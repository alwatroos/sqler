// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Completion calls as the app serves them: at most one call per plugin in flight, and one future per
//! plugin that is free. A busy plugin is skipped for that ask, so calls never pile up behind the
//! plugin's lock; it gets a future that resolves when its call in flight ends, so the caller can ask
//! it again.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use sqler_driver_api::BoxFuture;
use sqler_plugin_api::{CompletionRequest, PluginId};
use sqler_plugin_host::PluginHost;
use sqler_ui::plugins::backend::{CompletionCalls, PluginCompletions};
use tokio::sync::oneshot;

use super::backend::one_line;

/// The plugins with a completion call in flight, each with the waiters for its release.
#[derive(Clone, Default)]
pub(super) struct CompletionGate(Arc<Mutex<HashMap<PluginId, Vec<oneshot::Sender<()>>>>>);

/// The right to call one plugin; the plugin is free again when it drops.
pub(super) struct Ticket {
    gate: CompletionGate,
    id: PluginId,
}

impl CompletionGate {
    /// A ticket for `id`, or `None` when a call to it is still in flight.
    pub(super) fn try_enter(&self, id: &PluginId) -> Option<Ticket> {
        let mut busy = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if busy.contains_key(id) {
            return None;
        }
        busy.insert(id.clone(), Vec::new());
        Some(Ticket {
            gate: self.clone(),
            id: id.clone(),
        })
    }

    /// A future that resolves when the call to `id` in flight ends (at once when none is).
    pub(super) fn released(&self, id: &PluginId) -> BoxFuture<'static, ()> {
        let mut busy = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let rx = busy.get_mut(id).map(|waiters| {
            let (tx, rx) = oneshot::channel();
            waiters.push(tx);
            rx
        });
        Box::pin(async move {
            if let Some(rx) = rx {
                // A dropped gate resolves it too.
                rx.await.ok();
            }
        })
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        let waiters = self
            .gate
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.id);
        for waiter in waiters.into_iter().flatten() {
            waiter.send(()).ok();
        }
    }
}

/// One future per free plugin of `providers` (id and display name), and a release future per busy
/// one. Each call future owns its ticket from its creation on, so one dropped before its first poll
/// frees the plugin too.
pub(super) fn fan_out(
    host: &PluginHost,
    providers: Vec<(PluginId, String)>,
    request: &CompletionRequest,
    gate: &CompletionGate,
) -> CompletionCalls {
    let mut out = CompletionCalls::default();
    for (plugin, source) in providers {
        let Some(ticket) = gate.try_enter(&plugin) else {
            let released = gate.released(&plugin);
            out.busy.push((plugin, released));
            continue;
        };
        let pending = host.complete(&plugin, request.clone());
        out.calls.push(Box::pin(async move {
            let _ticket = ticket;
            match pending.await {
                Ok(items) => Ok(PluginCompletions {
                    plugin,
                    source,
                    items,
                }),
                Err(e) => Err(one_line(&e.to_string())),
            }
        }));
    }
    out
}

#[cfg(test)]
mod tests {
    use std::future::{Future, ready};
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    use super::*;

    fn id(s: &str) -> PluginId {
        s.parse().expect("a plugin id")
    }

    /// A busy plugin is skipped; another one is not.
    #[test]
    fn the_gate_skips_a_busy_plugin() {
        let gate = CompletionGate::default();
        let held = gate.try_enter(&id("alpha"));
        assert!(held.is_some());
        assert!(gate.try_enter(&id("alpha")).is_none());
        assert!(gate.try_enter(&id("beta")).is_some());
    }

    /// The ticket frees the plugin when it drops.
    #[test]
    fn a_dropped_ticket_frees_the_plugin() {
        let gate = CompletionGate::default();
        drop(gate.try_enter(&id("alpha")));
        assert!(gate.try_enter(&id("alpha")).is_some());
    }

    /// The release future of a busy plugin resolves when its ticket drops, and at once for a free one.
    #[test]
    fn a_release_future_resolves_when_the_busy_call_ends() {
        let gate = CompletionGate::default();
        let mut cx = Context::from_waker(Waker::noop());
        let mut free = gate.released(&id("alpha"));
        assert_eq!(pin!(&mut free).poll(&mut cx), Poll::Ready(()));
        let ticket = gate.try_enter(&id("alpha")).expect("free");
        let mut waiting = gate.released(&id("alpha"));
        assert_eq!(pin!(&mut waiting).poll(&mut cx), Poll::Pending);
        drop(ticket);
        assert_eq!(pin!(&mut waiting).poll(&mut cx), Poll::Ready(()));
        assert!(gate.try_enter(&id("alpha")).is_some());
    }

    /// A future that owns its ticket frees the plugin when it completes, and when it is dropped
    /// without ever being polled.
    #[test]
    fn a_ticket_is_freed_on_completion_and_on_an_unpolled_drop() {
        let gate = CompletionGate::default();
        let owning = |ticket: Ticket| {
            Box::pin(async move {
                let _ticket = ticket;
                ready(()).await;
            })
        };
        let ticket = gate.try_enter(&id("alpha")).expect("free");
        let mut done = owning(ticket);
        assert!(gate.try_enter(&id("alpha")).is_none());
        let mut cx = Context::from_waker(Waker::noop());
        assert_eq!(pin!(&mut done).poll(&mut cx), Poll::Ready(()));
        drop(done);
        let ticket = gate.try_enter(&id("alpha")).expect("freed on completion");
        let unpolled = owning(ticket);
        assert!(gate.try_enter(&id("alpha")).is_none());
        drop(unpolled);
        assert!(gate.try_enter(&id("alpha")).is_some());
    }
}
