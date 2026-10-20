// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `--open-sessions`: opens every saved connection through the normal connect pipeline
//! (ADR-0016), then idles, so the idle RSS with sessions open can be measured (AGENTS.md §3).
//!
//! Nothing here prompts or prints a secret. A profile with a keychain secret is skipped before
//! the connector is called (reading the keychain can prompt), and `trust_host_key` is never
//! called, so an unknown or changed SSH host key is a logged skip. Skip lines contain the connection
//! name and, for SSH failures, the jump host and port from the connect failure text; never the
//! database target, a credential or key material.

use std::sync::Arc;

use sqler_config::ConnectionProfile;
use sqler_driver_api::Session;
use sqler_ui::connections::{ConnectRequest, Connector, SecretSource};
use tokio::runtime::Handle;

/// Opens every profile through `connector`, logging one line per profile and a summary.
pub(crate) async fn open_all(
    connector: &dyn Connector,
    profiles: &[ConnectionProfile],
    log: &mut (dyn FnMut(String) + Send),
) -> Vec<Box<dyn Session>> {
    let mut sessions: Vec<Box<dyn Session>> = Vec::new();
    for profile in profiles {
        let name = &profile.spec().name;
        let request = match ConnectRequest::from_profile(profile) {
            Ok(request) => request,
            Err(err) => {
                log(format!("sqler: open-sessions: skipped {name:?}: {err}"));
                continue;
            }
        };
        if matches!(request.db_password, SecretSource::Keychain(_))
            || matches!(request.ssh_secret, SecretSource::Keychain(_))
        {
            log(format!(
                "sqler: open-sessions: skipped {name:?}: a secret is in the keychain (reading it may prompt)"
            ));
            continue;
        }
        match connector.connect(request).await {
            Ok(session) => {
                sessions.push(session);
                log(format!("sqler: open-sessions: opened {name:?}"));
            }
            Err(failure) => log(format!("sqler: open-sessions: skipped {name:?}: {failure}")),
        }
    }
    log(format!(
        "sqler: open-sessions: {} of {} open, idling",
        sessions.len(),
        profiles.len()
    ));
    sessions
}

/// The line logged when the saved connections did not load (never the error text).
const LOAD_FAILED: &str = "sqler: open-sessions: the saved connections could not be loaded";

/// Opens the loaded profiles; when the store did not load, logs `LOAD_FAILED` only.
async fn open_loaded(
    connector: &dyn Connector,
    profiles: Result<Vec<ConnectionProfile>, ()>,
    log: &mut (impl FnMut(String) + Send),
) -> Vec<Box<dyn Session>> {
    if let Ok(profiles) = profiles {
        open_all(connector, &profiles, log).await
    } else {
        log(LOAD_FAILED.to_owned());
        Vec::new()
    }
}

/// Spawns `open_loaded` (logging with `eprintln!`) on `handle`; the task then holds the sessions
/// with `std::future::pending::<()>().await`. `profiles = Err(())` logs the load line only.
pub(crate) fn spawn(
    handle: &Handle,
    connector: Arc<dyn Connector>,
    profiles: Result<Vec<ConnectionProfile>, ()>,
) {
    handle.spawn(async move {
        let mut log = |line: String| eprintln!("{line}");
        let _sessions = open_loaded(connector.as_ref(), profiles, &mut log).await;
        // Hold the sessions open for the measurement.
        std::future::pending::<()>().await;
    });
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use sqler_config::{ConnectionSpec, ConnectionStore, SshAuth, SshProfile};
    use sqler_driver_api::sqler_core::{DriverId, Query};
    use sqler_driver_api::{BoxFuture, CancelFlag, CancelHandle, DriverError, QueryStream};
    use sqler_ui::connections::{ConnectFailure, HostKeyPrompt};

    use super::*;

    struct FakeSession;

    impl Session for FakeSession {
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

    #[derive(Clone, Copy)]
    enum Fail {
        UnknownHostKey,
        Driver,
    }

    #[derive(Default)]
    struct FakeConnector {
        requests: Mutex<Vec<String>>,
        failing: Vec<(String, Fail)>,
        trusted: Arc<AtomicUsize>,
    }

    fn prompt() -> HostKeyPrompt {
        HostKeyPrompt {
            host: "bastion.example".into(),
            port: 22,
            algorithm: "ssh-ed25519".into(),
            fingerprint: "SHA256:abc".into(),
            public_key: "ssh-ed25519 AAAA".into(),
        }
    }

    impl Connector for FakeConnector {
        fn driver_ids(&self) -> Vec<DriverId> {
            Vec::new()
        }

        fn connect(
            &self,
            request: ConnectRequest,
        ) -> BoxFuture<'static, Result<Box<dyn Session>, ConnectFailure>> {
            let name = request.spec.name.clone();
            self.requests.lock().unwrap().push(name.clone());
            let fail = self
                .failing
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, f)| *f);
            Box::pin(async move {
                match fail {
                    None => Ok(Box::new(FakeSession) as Box<dyn Session>),
                    Some(Fail::UnknownHostKey) => Err(ConnectFailure::UnknownHostKey(prompt())),
                    Some(Fail::Driver) => Err(ConnectFailure::Driver(DriverError::Connection(
                        "refused".into(),
                    ))),
                }
            })
        }

        fn trust_host_key(
            &self,
            _key: HostKeyPrompt,
        ) -> BoxFuture<'static, Result<(), ConnectFailure>> {
            self.trusted.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        }
    }

    fn driver() -> DriverId {
        DriverId::new("postgres").unwrap()
    }

    fn profiles(specs: Vec<ConnectionSpec>) -> Vec<ConnectionProfile> {
        let mut store = ConnectionStore::new();
        for spec in specs {
            store.add_connection(spec, None).unwrap();
        }
        store.connections().to_vec()
    }

    fn plain(name: &str) -> ConnectionSpec {
        ConnectionSpec::new(name, driver(), "postgres://h/d?user=u")
    }

    async fn run(
        connector: &FakeConnector,
        profiles: &[ConnectionProfile],
    ) -> (usize, Vec<String>) {
        let mut lines = Vec::new();
        let sessions = open_all(connector, profiles, &mut |l| lines.push(l)).await;
        (sessions.len(), lines)
    }

    #[tokio::test]
    async fn plain_profiles_open() {
        let connector = FakeConnector::default();
        let (n, lines) = run(&connector, &profiles(vec![plain("a"), plain("b")])).await;
        assert_eq!(n, 2);
        assert_eq!(
            lines,
            [
                "sqler: open-sessions: opened \"a\"",
                "sqler: open-sessions: opened \"b\"",
                "sqler: open-sessions: 2 of 2 open, idling",
            ]
        );
    }

    #[tokio::test]
    async fn keychain_secrets_are_skipped_without_calling_the_connector() {
        let connector = FakeConnector::default();
        let ssh = SshProfile::new("bastion", "me", SshAuth::Password);
        let list = profiles(vec![
            plain("db").with_password_in_keychain(true),
            plain("tunnel").with_ssh(ssh),
            plain("ok"),
        ]);
        let (n, lines) = run(&connector, &list).await;
        assert_eq!(n, 1);
        assert_eq!(*connector.requests.lock().unwrap(), ["ok"]);
        let skip = "a secret is in the keychain (reading it may prompt)";
        assert_eq!(
            lines[0],
            format!("sqler: open-sessions: skipped \"db\": {skip}")
        );
        assert_eq!(
            lines[1],
            format!("sqler: open-sessions: skipped \"tunnel\": {skip}")
        );
        assert_eq!(lines[3], "sqler: open-sessions: 1 of 3 open, idling");
    }

    #[tokio::test]
    async fn unknown_host_key_is_a_skip_and_never_trusted() {
        let connector = FakeConnector {
            failing: vec![("t".into(), Fail::UnknownHostKey)],
            ..FakeConnector::default()
        };
        let (n, lines) = run(&connector, &profiles(vec![plain("t")])).await;
        assert_eq!(n, 0);
        assert_eq!(
            lines[0],
            "sqler: open-sessions: skipped \"t\": the SSH host key of bastion.example:22 is not trusted yet"
        );
        assert_eq!(connector.trusted.load(Ordering::SeqCst), 0);
        assert!(!lines.iter().any(|l| l.contains("AAAA")));
    }

    #[tokio::test]
    async fn lines_never_contain_the_target_or_a_secret() {
        let connector = FakeConnector {
            failing: vec![("p".into(), Fail::Driver)],
            ..FakeConnector::default()
        };
        let spec = ConnectionSpec::new("p", driver(), "postgres://db.internal/d?user=u");
        let (_, lines) = run(&connector, &profiles(vec![spec])).await;
        assert_eq!(
            lines[0],
            "sqler: open-sessions: skipped \"p\": connection failed: refused"
        );
        for line in &lines {
            assert!(!line.contains("hunter2"));
            assert!(!line.contains("db.internal"));
            assert!(!line.contains("postgres://"));
            assert!(!line.contains("AAAA"));
        }
    }

    #[test]
    fn load_line_has_no_error_text() {
        assert_eq!(
            LOAD_FAILED,
            "sqler: open-sessions: the saved connections could not be loaded"
        );
    }

    #[tokio::test]
    async fn an_unloaded_store_logs_only_the_load_line() {
        let mut lines = Vec::new();
        let sessions =
            open_loaded(&FakeConnector::default(), Err(()), &mut |l| lines.push(l)).await;
        assert!(sessions.is_empty());
        assert_eq!(lines, vec![LOAD_FAILED.to_owned()]);
    }

    #[tokio::test]
    async fn spawn_with_an_unloaded_store_does_not_panic() {
        spawn(
            &Handle::current(),
            Arc::new(FakeConnector::default()),
            Err(()),
        );
        tokio::task::yield_now().await;
    }
}
