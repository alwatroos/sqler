// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

use std::collections::BTreeMap;

use sqler_core::{ConnectionConfig, DriverId};

use crate::{Driver, DriverError, Session};

/// Maps driver ids to drivers.
#[derive(Default)]
pub struct DriverRegistry {
    drivers: BTreeMap<DriverId, Box<dyn Driver>>,
}

impl DriverRegistry {
    /// Creates an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a driver under its own id.
    ///
    /// # Errors
    /// Returns `DriverError::Core(CoreError::InvalidDriverId)` for a malformed id and
    /// [`DriverError::DuplicateDriver`] if the id is already registered.
    pub fn register(&mut self, driver: Box<dyn Driver>) -> Result<(), DriverError> {
        let id = DriverId::new(driver.id()).map_err(DriverError::from)?;
        if self.drivers.contains_key(&id) {
            return Err(DriverError::DuplicateDriver(id.to_string()));
        }
        self.drivers.insert(id, driver);
        Ok(())
    }

    /// The driver registered under `id`.
    #[must_use]
    pub fn get(&self, id: &DriverId) -> Option<&dyn Driver> {
        self.drivers.get(id).map(AsRef::as_ref)
    }

    /// Registered ids in sorted order.
    pub fn ids(&self) -> impl Iterator<Item = &DriverId> {
        self.drivers.keys()
    }

    /// Opens a session with the driver named in `config`.
    ///
    /// # Errors
    /// Returns [`DriverError::UnknownDriver`] if no driver matches, or the driver's own error.
    pub async fn connect(
        &self,
        config: &ConnectionConfig,
    ) -> Result<Box<dyn Session>, DriverError> {
        let driver = self
            .get(&config.driver)
            .ok_or_else(|| DriverError::UnknownDriver(config.driver.to_string()))?;
        driver.connect(config).await
    }
}

impl std::fmt::Debug for DriverRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.ids()).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::tests::block_on;
    use crate::{BoxFuture, CancelFlag, CancelHandle, QueryStream};
    use sqler_core::{CoreError, Query};

    struct NullSession;
    impl Session for NullSession {
        fn cancel_handle(&self) -> CancelHandle {
            CancelFlag::new().handle()
        }
        fn execute<'s>(
            &'s mut self,
            _query: &'s Query,
        ) -> BoxFuture<'s, Result<Box<dyn QueryStream + 's>, DriverError>> {
            Box::pin(async { Err(DriverError::Unsupported("test".into())) })
        }
        fn close(self: Box<Self>) -> BoxFuture<'static, Result<(), DriverError>> {
            Box::pin(async { Ok(()) })
        }
    }

    struct Fake(&'static str);
    impl Driver for Fake {
        fn id(&self) -> &'static str {
            self.0
        }
        fn connect<'a>(
            &'a self,
            _config: &'a ConnectionConfig,
        ) -> BoxFuture<'a, Result<Box<dyn Session>, DriverError>> {
            Box::pin(async { Ok(Box::new(NullSession) as Box<dyn Session>) })
        }
    }

    fn cfg(driver: &str) -> ConnectionConfig {
        ConnectionConfig::new("t", DriverId::new(driver).unwrap(), "x")
    }

    #[test]
    fn register_and_get() {
        let mut r = DriverRegistry::new();
        r.register(Box::new(Fake("fake"))).unwrap();
        let id = DriverId::new("fake").unwrap();
        assert_eq!(r.get(&id).unwrap().id(), "fake");
        assert!(r.get(&DriverId::new("other").unwrap()).is_none());
    }

    #[test]
    fn duplicate_is_rejected() {
        let mut r = DriverRegistry::new();
        r.register(Box::new(Fake("fake"))).unwrap();
        let err = r.register(Box::new(Fake("fake"))).unwrap_err();
        assert!(matches!(err, DriverError::DuplicateDriver(ref s) if s == "fake"));
    }

    #[test]
    fn invalid_id_is_rejected() {
        let mut r = DriverRegistry::new();
        let err = r.register(Box::new(Fake("Bad Id"))).unwrap_err();
        assert!(matches!(
            err,
            DriverError::Core(CoreError::InvalidDriverId(_))
        ));
    }

    #[test]
    fn ids_are_sorted() {
        let mut r = DriverRegistry::new();
        for id in ["zeta", "alpha", "mid"] {
            r.register(Box::new(Fake(id))).unwrap();
        }
        let ids: Vec<&str> = r.ids().map(DriverId::as_str).collect();
        assert_eq!(ids, ["alpha", "mid", "zeta"]);
        assert_eq!(
            format!("{r:?}"),
            r#"[DriverId("alpha"), DriverId("mid"), DriverId("zeta")]"#
        );
    }

    #[test]
    fn connect_dispatches_and_unknown_fails() {
        let mut r = DriverRegistry::new();
        r.register(Box::new(Fake("fake"))).unwrap();
        assert!(block_on(r.connect(&cfg("fake"))).is_ok());
        let err = block_on(r.connect(&cfg("nope"))).err().unwrap();
        assert!(matches!(err, DriverError::UnknownDriver(ref s) if s == "nope"));
    }
}
