// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

use std::collections::BTreeMap;
use std::sync::Arc;

use sqler_core::{ConnectionConfig, DriverId};

use crate::{Driver, DriverError, Session};

/// What a [`DriverSource`] knows about one driver id.
#[derive(Clone)]
#[non_exhaustive]
pub enum SourceLookup {
    /// The source does not provide this id.
    Unknown,
    /// The source provides the driver and it can be used now.
    Available(Arc<dyn Driver>),
    /// The source provides the id but it cannot be used now; the text is shown to the user.
    Unavailable(String),
}

impl std::fmt::Debug for SourceLookup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown => f.write_str("Unknown"),
            Self::Available(d) => f.debug_tuple("Available").field(&d.id()).finish(),
            Self::Unavailable(t) => f.debug_tuple("Unavailable").field(t).finish(),
        }
    }
}

/// A run-time source of drivers that is asked after the built-in drivers (for example plugins).
pub trait DriverSource: Send + Sync {
    /// The ids available now. May block: never call it on the UI thread.
    fn ids(&self) -> Vec<DriverId>;

    /// What the source knows about `id`.
    fn lookup(&self, id: &DriverId) -> SourceLookup;
}

/// Maps driver ids to drivers, plus run-time [`DriverSource`]s consulted after the built-ins.
#[derive(Default)]
pub struct DriverRegistry {
    drivers: BTreeMap<DriverId, Box<dyn Driver>>,
    sources: Vec<Arc<dyn DriverSource>>,
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

    /// Built-in ids in sorted order. Ids from [`DriverSource`]s are not included: see
    /// [`available_ids`](Self::available_ids).
    pub fn ids(&self) -> impl Iterator<Item = &DriverId> {
        self.drivers.keys()
    }

    /// Adds a run-time source. Sources are asked in insertion order, after the built-in drivers.
    pub fn add_source(&mut self, source: Arc<dyn DriverSource>) {
        self.sources.push(source);
    }

    /// Built-in ids plus the ids of every source, sorted and without duplicates. May block (it asks the
    /// sources): never call it on the UI thread.
    #[must_use]
    pub fn available_ids(&self) -> Vec<DriverId> {
        let mut ids: Vec<DriverId> = self.drivers.keys().cloned().collect();
        for source in &self.sources {
            ids.extend(source.ids());
        }
        ids.sort();
        ids.dedup();
        ids
    }

    /// Opens a session with the driver named in `config`: the built-in driver first, then the sources in
    /// insertion order.
    ///
    /// # Errors
    /// Returns [`DriverError::Unavailable`] when a source owns the id but cannot serve it,
    /// [`DriverError::UnknownDriver`] if no driver matches, or the driver's own error.
    pub async fn connect(
        &self,
        config: &ConnectionConfig,
    ) -> Result<Box<dyn Session>, DriverError> {
        if let Some(driver) = self.get(&config.driver) {
            return driver.connect(config).await;
        }
        for source in &self.sources {
            match source.lookup(&config.driver) {
                SourceLookup::Available(driver) => return driver.connect(config).await,
                SourceLookup::Unavailable(text) => return Err(DriverError::Unavailable(text)),
                SourceLookup::Unknown => {}
            }
        }
        Err(DriverError::UnknownDriver(config.driver.to_string()))
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

    struct Src {
        ids: Vec<&'static str>,
        lookup: fn(&str) -> SourceLookup,
    }
    impl DriverSource for Src {
        fn ids(&self) -> Vec<DriverId> {
            self.ids.iter().map(|i| DriverId::new(i).unwrap()).collect()
        }
        fn lookup(&self, id: &DriverId) -> SourceLookup {
            (self.lookup)(id.as_str())
        }
    }

    fn avail(name: &str) -> SourceLookup {
        match name {
            "plug" => SourceLookup::Available(Arc::new(Fake("plug"))),
            "fake" => SourceLookup::Available(Arc::new(Fake("fake"))),
            "down" => SourceLookup::Unavailable("it is off".into()),
            _ => SourceLookup::Unknown,
        }
    }

    fn src(ids: Vec<&'static str>) -> Arc<Src> {
        Arc::new(Src { ids, lookup: avail })
    }

    fn names(ids: &[DriverId]) -> Vec<String> {
        ids.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn available_ids_merge_sort_dedupe() {
        let mut r = DriverRegistry::new();
        r.register(Box::new(Fake("zeta"))).unwrap();
        r.register(Box::new(Fake("fake"))).unwrap();
        r.add_source(src(vec!["plug", "fake"]));
        r.add_source(src(vec!["alpha", "plug"]));
        assert_eq!(names(&r.available_ids()), ["alpha", "fake", "plug", "zeta"]);
        let built_in: Vec<&str> = r.ids().map(DriverId::as_str).collect();
        assert_eq!(built_in, ["fake", "zeta"]);
        assert_eq!(format!("{r:?}"), r#"[DriverId("fake"), DriverId("zeta")]"#);
    }

    #[test]
    fn source_connects_and_builtin_wins() {
        struct Marker;
        impl Driver for Marker {
            fn id(&self) -> &'static str {
                "fake"
            }
            fn connect<'a>(
                &'a self,
                _config: &'a ConnectionConfig,
            ) -> BoxFuture<'a, Result<Box<dyn Session>, DriverError>> {
                Box::pin(async { Err(DriverError::Connection("built-in".into())) })
            }
        }
        let mut r = DriverRegistry::new();
        r.register(Box::new(Marker)).unwrap();
        r.add_source(src(vec!["plug", "fake"]));
        assert!(block_on(r.connect(&cfg("plug"))).is_ok());
        let err = block_on(r.connect(&cfg("fake"))).err().unwrap();
        assert!(matches!(err, DriverError::Connection(ref s) if s == "built-in"));
    }

    #[test]
    fn unavailable_source_text_and_source_order() {
        let mut r = DriverRegistry::new();
        r.add_source(src(vec!["down"]));
        r.add_source(Arc::new(Src {
            ids: vec!["down"],
            lookup: |_| SourceLookup::Available(Arc::new(Fake("late"))),
        }));
        let err = block_on(r.connect(&cfg("down"))).err().unwrap();
        assert!(matches!(err, DriverError::Unavailable(ref s) if s == "it is off"));
        assert_eq!(err.to_string(), "driver unavailable: it is off");
        // A later source is reached for ids the earlier ones do not know.
        assert!(block_on(r.connect(&cfg("nope"))).is_ok());
    }

    #[test]
    fn unknown_everywhere_is_unknown_driver() {
        let mut r = DriverRegistry::new();
        r.add_source(Arc::new(Src {
            ids: vec![],
            lookup: |_| SourceLookup::Unknown,
        }));
        let err = block_on(r.connect(&cfg("nope"))).err().unwrap();
        assert!(matches!(err, DriverError::UnknownDriver(ref s) if s == "nope"));
    }
}
