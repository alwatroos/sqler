// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The session source of the editor assistant: the catalog the browser already loaded and no
//! session. The assistant therefore never runs SQL, not even a read (ADR-0052).

use sqler_ai_api::BoxFuture;
use sqler_ai_tools::{ConnectionInfo, SessionSource, ToolError};
use sqler_catalog::Catalog;
use sqler_driver_api::Session;

/// One connection and, when its browser is ready, its catalog.
pub struct EditorSource {
    info: ConnectionInfo,
    catalog: Option<Catalog>,
}

impl EditorSource {
    /// A source for `info`. Without a `catalog` the request runs ungrounded.
    #[must_use]
    pub fn new(info: ConnectionInfo, catalog: Option<Catalog>) -> Self {
        Self { info, catalog }
    }
}

impl SessionSource for EditorSource {
    fn connections(&self) -> Vec<ConnectionInfo> {
        vec![self.info.clone()]
    }

    fn catalog(&self, _connection: &str) -> Option<Catalog> {
        self.catalog.clone()
    }

    fn acquire<'a>(
        &'a self,
        _connection: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn Session>, ToolError>> {
        Box::pin(std::future::ready(Err(ToolError::NotAvailable(
            "the editor assistant reads the catalog only".to_owned(),
        ))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqler_driver_api::sqler_core::DriverId;

    fn info() -> ConnectionInfo {
        ConnectionInfo::new("dev", "Dev", DriverId::new("sqlite").unwrap(), None)
    }

    #[test]
    fn lends_the_connection_and_no_session() {
        let source = EditorSource::new(info(), None);
        assert_eq!(source.connections(), vec![info()]);
        assert!(source.catalog("dev").is_none());
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let result = rt.block_on(source.acquire("dev"));
        assert!(matches!(result, Err(ToolError::NotAvailable(_))));
    }
}
