// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::{BoxFuture, DriverError};

/// Backend-specific way to cancel a running operation.
pub trait Canceller: Send + Sync {
    /// Requests cancellation of the running operation.
    fn cancel(&self) -> BoxFuture<'_, Result<(), DriverError>>;
}

/// Cloneable handle that cancels the operation running on a session.
///
/// After a cancel the result stream yields `Err(DriverError::Cancelled)` and then ends.
#[derive(Clone)]
pub struct CancelHandle(Arc<dyn Canceller>);

impl CancelHandle {
    /// Wraps a canceller.
    #[must_use]
    pub fn new(canceller: Arc<dyn Canceller>) -> Self {
        Self(canceller)
    }

    /// Requests cancellation.
    ///
    /// # Errors
    /// Returns the backend's error if the cancel request itself fails.
    pub async fn cancel(&self) -> Result<(), DriverError> {
        self.0.cancel().await
    }
}

impl std::fmt::Debug for CancelHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CancelHandle")
    }
}

/// A shared flag for cooperative cancellation.
///
/// Drivers without an async interrupt (such as `SQLite`) check it between batches.
#[derive(Debug, Clone, Default)]
pub struct CancelFlag(Arc<AtomicBool>);

impl CancelFlag {
    /// Creates a flag that is not cancelled.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the flag.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// Whether [`CancelFlag::cancel`] was called on this flag or a clone.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    /// A [`CancelHandle`] that sets this flag.
    #[must_use]
    pub fn handle(&self) -> CancelHandle {
        CancelHandle::new(Arc::new(self.clone()))
    }
}

impl Canceller for CancelFlag {
    fn cancel(&self) -> BoxFuture<'_, Result<(), DriverError>> {
        CancelFlag::cancel(self);
        Box::pin(std::future::ready(Ok(())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::tests::block_on;

    #[test]
    fn default_is_false() {
        assert!(!CancelFlag::new().is_cancelled());
        assert!(!CancelFlag::default().is_cancelled());
    }

    #[test]
    fn cancel_sets_and_clones_share() {
        let f = CancelFlag::new();
        let g = f.clone();
        f.cancel();
        assert!(f.is_cancelled());
        assert!(g.is_cancelled());
    }

    #[test]
    fn handle_sets_flag() {
        let f = CancelFlag::new();
        let h = f.handle();
        let h2 = h.clone();
        block_on(h2.cancel()).unwrap();
        assert!(f.is_cancelled());
        assert_eq!(format!("{h:?}"), "CancelHandle");
    }
}
