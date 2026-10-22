// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Calls into a plugin: one task per call on the injected runtime, serialised per plugin, bounded by a deadline and a
//! cancel token. Any fault drops only that plugin's instance.

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use sqler_plugin_api::PluginId;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use super::host::{Inner, MAX_FAULTS, PluginEntry, PluginHost, shut_down_error};
use super::instance::{Live, bounded, classify, instantiate};
use crate::error::CallError;
use crate::status::{CallKind, DisableReason};

/// What a call runs on the live instance. Later items build it from the generated `GuestIndices`.
pub(crate) type CallFn<R> = Box<
    dyn for<'a> FnOnce(
            &'a mut Live,
        ) -> Pin<Box<dyn Future<Output = wasmtime::Result<R>> + Send + 'a>>
        + Send,
>;

/// A call in flight. A future on any executor; [`cancel`](Self::cancel) stops it. Dropping it lets the call finish
/// (bounded by its deadline) and discards the result.
#[derive(Debug)]
#[must_use = "a call that is neither awaited nor cancelled still runs, and its result is lost"]
pub struct PendingCall<R> {
    rx: oneshot::Receiver<Result<R, CallError>>,
    cancel: CancellationToken,
}

impl<R> PendingCall<R> {
    /// Cancels the call: it ends with [`CallError::Cancelled`] and its plugin instance is dropped.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    fn failed(e: CallError) -> Self {
        let (tx, rx) = oneshot::channel();
        // The receiver is right here; sending cannot fail.
        let _ = tx.send(Err(e));
        Self {
            rx,
            cancel: CancellationToken::new(),
        }
    }
}

impl<R> Future for PendingCall<R> {
    type Output = Result<R, CallError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.rx)
            .poll(cx)
            // The task vanished without answering: the runtime shut down under it.
            .map(|r| r.unwrap_or(Err(CallError::Cancelled)))
    }
}

/// A call's result and whether it counts as a fault.
struct Done<R> {
    result: Result<R, CallError>,
    fault: bool,
}

impl<R> Done<R> {
    fn err(error: CallError, fault: bool) -> Self {
        Self {
            result: Err(error),
            fault,
        }
    }
}

impl PluginHost {
    /// Runs `f` on the live instance of `id`, instantiating it first when needed.
    pub(crate) fn run<R: Send + 'static>(
        &self,
        id: &PluginId,
        kind: CallKind,
        f: CallFn<R>,
    ) -> PendingCall<R> {
        let inner = &self.inner;
        if inner.shutdown.load(std::sync::atomic::Ordering::SeqCst) {
            return PendingCall::failed(shut_down_error());
        }
        let Some(entry) = inner.entry(id) else {
            return PendingCall::failed(CallError::NotFound);
        };
        if let Some(e) = entry.disabled_error() {
            return PendingCall::failed(e);
        }
        let cancel = entry.call_token();
        let (tx, rx) = oneshot::channel();
        let work = inner.opts.runtime.spawn(execute(
            Arc::clone(inner),
            Arc::clone(&entry),
            kind,
            f,
            cancel.clone(),
        ));
        inner.opts.runtime.spawn(async move {
            let done = match work.await {
                Ok(done) => done,
                Err(e) if e.is_panic() => {
                    // The panic left the instance in an unknown state. Counted under the plugin lock, like every
                    // other fault, so a queued call sees the result.
                    let mut live = entry.live.lock().await;
                    *live = None;
                    let done = Done::err(CallError::HostPanic, true);
                    account(&entry, &done);
                    done
                }
                Err(_) => Done::err(CallError::Cancelled, false),
            };
            // The caller may have dropped the `PendingCall`; the result is then discarded.
            let _ = tx.send(done.result);
        });
        PendingCall { rx, cancel }
    }
}

async fn execute<R: Send + 'static>(
    inner: Arc<Inner>,
    entry: Arc<PluginEntry>,
    kind: CallKind,
    f: CallFn<R>,
    cancel: CancellationToken,
) -> Done<R> {
    let mut guard = tokio::select! {
        biased;
        () = cancel.cancelled() => return Done::err(CallError::Cancelled, false),
        g = entry.live.lock() => g,
    };
    // Faults are counted before the lock is released: the next queued call must see a disable the third fault caused.
    let done = locked(&inner, &entry, kind, f, &cancel, &mut guard).await;
    account(&entry, &done);
    done
}

/// A call with the plugin lock held.
async fn locked<R: Send + 'static>(
    inner: &Arc<Inner>,
    entry: &Arc<PluginEntry>,
    kind: CallKind,
    f: CallFn<R>,
    cancel: &CancellationToken,
    guard: &mut Option<Live>,
) -> Done<R> {
    // Shut down, disabled or refreshed while this call waited its turn.
    if inner.shutdown.load(std::sync::atomic::Ordering::SeqCst) {
        return Done::err(shut_down_error(), false);
    }
    if let Some(e) = entry.disabled_error() {
        return Done::err(e, false);
    }
    if guard
        .as_ref()
        .is_some_and(|l| l.generation != entry.generation())
    {
        *guard = None;
    }
    if guard.is_none() {
        match instantiate(inner, entry, cancel).await {
            Ok(live) => {
                *guard = Some(live);
                entry.set_live(true);
            }
            Err(fail) => return Done::err(fail.error, fail.fault),
        }
    }
    let Some(live) = guard.as_mut() else {
        return Done::err(
            CallError::Engine {
                message: "no instance".to_owned(),
            },
            false,
        );
    };
    // The engine exists: the instance was built on it. Refused once it is stopped, before any guest code runs.
    let Some(Ok(_flight)) = inner.engine().ok().map(|h| h.in_flight()) else {
        *guard = None;
        entry.set_live(false);
        return Done::err(shut_down_error(), false);
    };
    // `shutdown` sets its flag before it replaces the kill token: a flag still clear here means the token this call
    // holds is a child of the one `shutdown` is about to cancel.
    if inner.shutdown.load(std::sync::atomic::Ordering::SeqCst) {
        *guard = None;
        entry.set_live(false);
        return Done::err(shut_down_error(), false);
    }
    let deadline = kind.deadline(&inner.opts.limits.deadlines);
    live.store.epoch_deadline_async_yield_and_update(1);
    let ran = bounded(deadline, cancel, f(live)).await;
    let result = classify(live.store.data(), deadline, ran);
    // A trapped, timed out, cancelled or otherwise failed instance is in an unknown state: drop it.
    let drop_it = matches!(
        &result,
        Err(CallError::Trap { .. }
            | CallError::MemoryLimit { .. }
            | CallError::Timeout { .. }
            | CallError::Cancelled
            | CallError::HostPanic
            | CallError::Engine { .. })
    );
    if drop_it {
        *guard = None;
        entry.set_live(false);
    }
    let fault = result.as_ref().err().is_some_and(CallError::is_fault);
    Done { result, fault }
}

/// Fault accounting: a success resets the count, the third consecutive fault disables the plugin.
fn account<R>(entry: &PluginEntry, done: &Done<R>) {
    let mut st = entry
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match &done.result {
        Ok(_) | Err(CallError::Guest(_)) if !done.fault => st.faults = 0,
        Err(CallError::Load(e)) => {
            st.live = false;
            st.disabled = Some(DisableReason::Load(e.clone()));
        }
        Err(e) if done.fault => {
            st.live = false;
            st.faults += 1;
            if st.faults >= MAX_FAULTS {
                st.disabled = Some(DisableReason::Faults {
                    count: st.faults,
                    last: e.clone(),
                });
            }
        }
        _ => {}
    }
}
