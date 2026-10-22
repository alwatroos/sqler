// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Cooperative cancellation of a session turn.

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Poll, Waker};

#[derive(Default)]
struct Shared {
    flag: AtomicBool,
    wakers: Mutex<Vec<Waker>>,
}

/// A handle that cancels the running turn of a session.
///
/// Cheap to clone; every clone cancels the same session. The engine races it at every await,
/// so a cancel takes effect at the next point the turn waits on anything.
#[derive(Clone, Default)]
pub struct SessionCancel(Arc<Shared>);

impl std::fmt::Debug for SessionCancel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionCancel")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

impl SessionCancel {
    /// Cancels the running turn. A cancel while no turn runs has no effect: `send` resets the
    /// flag when a turn begins.
    pub fn cancel(&self) {
        self.0.flag.store(true, Ordering::SeqCst);
        let wakers =
            std::mem::take(&mut *self.0.wakers.lock().unwrap_or_else(PoisonError::into_inner));
        for w in wakers {
            w.wake();
        }
    }

    /// Whether `cancel` was called since the turn began.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.flag.load(Ordering::SeqCst)
    }

    /// Clears the flag; a new turn starts uncancelled.
    pub(crate) fn reset(&self) {
        self.0.flag.store(false, Ordering::SeqCst);
        self.0
            .wakers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }

    fn register(&self, waker: &Waker) {
        let mut list = self.0.wakers.lock().unwrap_or_else(PoisonError::into_inner);
        if !list.iter().any(|w| w.will_wake(waker)) {
            list.push(waker.clone());
        }
    }
}

/// The marker error of a cancelled await.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Cancelled;

/// Polls `fut` until it is ready or `cancel` fires, whichever comes first. The waker is
/// registered before the flag is read, so a cancel cannot slip between the two.
pub(crate) async fn cancellable<F: Future>(
    cancel: &SessionCancel,
    fut: F,
) -> Result<F::Output, Cancelled> {
    let mut fut = std::pin::pin!(fut);
    std::future::poll_fn(|cx| {
        cancel.register(cx.waker());
        if cancel.is_cancelled() {
            return Poll::Ready(Err(Cancelled));
        }
        fut.as_mut().poll(cx).map(Ok)
    })
    .await
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn cancel_before_wins() {
        let c = SessionCancel::default();
        c.cancel();
        assert!(c.is_cancelled());
        let r = cancellable(&c, std::future::ready(1)).await;
        assert_eq!(r, Err(Cancelled));
    }

    #[tokio::test]
    async fn cancel_during_wakes() {
        let c = SessionCancel::default();
        let other = c.clone();
        let task =
            tokio::spawn(async move { cancellable(&other, std::future::pending::<()>()).await });
        tokio::time::sleep(Duration::from_millis(20)).await;
        c.cancel();
        assert_eq!(task.await.unwrap(), Err(Cancelled));
    }

    #[tokio::test]
    async fn never_cancelled_passes_through() {
        let c = SessionCancel::default();
        assert_eq!(cancellable(&c, async { 7 }).await, Ok(7));
        c.cancel();
        c.reset();
        assert!(!c.is_cancelled());
        assert_eq!(cancellable(&c, async { 8 }).await, Ok(8));
    }
}
