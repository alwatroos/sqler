// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Completion calls as the app serves them: at most one call per plugin in flight, and one future per
//! plugin that is free. A busy plugin is skipped for that ask, so calls never pile up behind the
//! plugin's lock.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};

use sqler_driver_api::BoxFuture;
use sqler_plugin_api::{CompletionRequest, PluginId};
use sqler_plugin_host::PluginHost;
use sqler_ui::plugins::backend::PluginCompletions;

use super::backend::one_line;

/// The plugins with a completion call in flight.
#[derive(Clone, Default)]
pub(super) struct CompletionGate(Arc<Mutex<HashSet<PluginId>>>);

/// The right to call one plugin; the plugin is free again when it drops.
pub(super) struct Ticket {
    gate: CompletionGate,
    id: PluginId,
}

impl CompletionGate {
    /// A ticket for `id`, or `None` when a call to it is still in flight.
    pub(super) fn try_enter(&self, id: &PluginId) -> Option<Ticket> {
        let mut busy = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        busy.insert(id.clone()).then(|| Ticket {
            gate: self.clone(),
            id: id.clone(),
        })
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        self.gate
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.id);
    }
}

/// One future per free plugin of `providers` (id and display name). Each future owns its ticket from
/// its creation on, so one dropped before its first poll frees the plugin too.
pub(super) fn fan_out(
    host: &PluginHost,
    providers: Vec<(PluginId, String)>,
    request: &CompletionRequest,
    gate: &CompletionGate,
) -> Vec<BoxFuture<'static, Result<PluginCompletions, String>>> {
    providers
        .into_iter()
        .filter_map(|(plugin, source)| {
            let ticket = gate.try_enter(&plugin)?;
            let pending = host.complete(&plugin, request.clone());
            let future: BoxFuture<'static, _> = Box::pin(async move {
                let _ticket = ticket;
                match pending.await {
                    Ok(items) => Ok(PluginCompletions {
                        plugin,
                        source,
                        items,
                    }),
                    Err(e) => Err(one_line(&e.to_string())),
                }
            });
            Some(future)
        })
        .collect()
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
