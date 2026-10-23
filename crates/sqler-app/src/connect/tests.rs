// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Pipeline tests with fakes: `MemoryStore`, a scripted tunnel opener, a recording driver.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use sqler_config::{
    MemoryStore, SecretRef, SshAuth, SshProfile, TlsMode as ProfileTlsMode, TlsProfile,
};
use sqler_driver_api::sqler_core::{Environment, Query, TlsMode};
use sqler_driver_api::{
    CancelFlag, CancelHandle, Driver, DriverError, DriverSource, QueryStream, SourceLookup,
};
use sqler_tunnel::TunnelError;

use super::tunnel::fake::FakeTunnels;
use super::*;

const PASSWORD: &str = "p@ss/w:rd";
const ENCODED: &str = "p%40ss%2Fw%3Ard";

struct Probe {
    configs: Arc<Mutex<Vec<ConnectionConfig>>>,
    session_closed: Arc<AtomicUsize>,
    fail: bool,
    hang: bool,
}

struct ProbeSession(Arc<AtomicUsize>);

impl Session for ProbeSession {
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
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }
}

impl Driver for Probe {
    fn id(&self) -> &'static str {
        "postgres"
    }

    fn connect<'a>(
        &'a self,
        config: &'a ConnectionConfig,
    ) -> BoxFuture<'a, Result<Box<dyn Session>, DriverError>> {
        self.configs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(config.clone());
        Box::pin(async move {
            if self.hang {
                std::future::pending::<()>().await;
            }
            if self.fail {
                return Err(DriverError::Connection("refused".into()));
            }
            Ok(Box::new(ProbeSession(Arc::clone(&self.session_closed))) as Box<dyn Session>)
        })
    }
}

struct Rig {
    pipeline: Pipeline,
    store: Arc<MemoryStore>,
    tunnels: Arc<FakeTunnels>,
    configs: Arc<Mutex<Vec<ConnectionConfig>>>,
    session_closed: Arc<AtomicUsize>,
}

impl Rig {
    fn new(fail: bool, hang: bool, known_hosts: Option<PathBuf>) -> Self {
        let configs = Arc::new(Mutex::new(Vec::new()));
        let session_closed = Arc::new(AtomicUsize::new(0));
        let mut registry = DriverRegistry::new();
        registry
            .register(Box::new(Probe {
                configs: Arc::clone(&configs),
                session_closed: Arc::clone(&session_closed),
                fail,
                hang,
            }))
            .unwrap();
        let store = Arc::new(MemoryStore::new());
        let tunnels = Arc::new(FakeTunnels::new(40_001));
        let pipeline = Pipeline::new(
            Arc::new(registry),
            Arc::clone(&store) as Arc<dyn SecretStore>,
            Arc::clone(&tunnels) as Arc<dyn TunnelOpener>,
            known_hosts,
            Handle::current(),
        );
        Self {
            pipeline,
            store,
            tunnels,
            configs,
            session_closed,
        }
    }

    fn ok() -> Self {
        Self::new(false, false, Some(PathBuf::from("known_hosts")))
    }

    fn targets(&self) -> Vec<String> {
        self.configs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|c| c.target.clone())
            .collect()
    }
}

fn driver() -> DriverId {
    DriverId::new("postgres").unwrap()
}

fn spec(target: &str) -> ConnectionSpec {
    ConnectionSpec::new("Prod DB", driver(), target)
}

fn ssh(auth: SshAuth) -> SshProfile {
    let mut ssh = SshProfile::new("bastion.example", "deploy", auth);
    ssh.port = 2222;
    ssh
}

fn request(spec: ConnectionSpec, db: SecretSource, ssh_secret: SecretSource) -> ConnectRequest {
    ConnectRequest {
        spec,
        db_password: db,
        ssh_secret,
    }
}

fn value(text: &str) -> SecretSource {
    SecretSource::Value(Secret::new(text))
}

fn reference(kind: SecretKind) -> SecretRef {
    SecretRef::new("prod-db", kind).unwrap()
}

async fn connect(rig: &Rig, request: ConnectRequest) -> Result<Box<dyn Session>, ConnectFailure> {
    rig.pipeline.connect(request).await
}

async fn fail(rig: &Rig, request: ConnectRequest) -> ConnectFailure {
    match connect(rig, request).await {
        Ok(_) => panic!("expected a failure"),
        Err(failure) => failure,
    }
}

async fn wait_for(counter: &AtomicUsize, expected: usize) {
    for _ in 0..100 {
        if counter.load(Ordering::SeqCst) == expected {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn driver_ids_are_listed() {
    assert_eq!(Rig::ok().pipeline.driver_ids(), vec![driver()]);
}

/// A source with one driver that is available (`plug-ok`, a recording `Probe`) and one that is not (`plug-off`).
struct FakeSource {
    configs: Arc<Mutex<Vec<ConnectionConfig>>>,
    closed: Arc<AtomicUsize>,
}

const OFF_TEXT: &str = "driver `plug-off` comes from plugin `p`, which is disabled";

impl DriverSource for FakeSource {
    fn ids(&self) -> Vec<DriverId> {
        vec![DriverId::new("plug-ok").unwrap()]
    }

    fn lookup(&self, id: &DriverId) -> SourceLookup {
        match id.as_str() {
            "plug-ok" => SourceLookup::Available(Arc::new(Probe {
                configs: Arc::clone(&self.configs),
                session_closed: Arc::clone(&self.closed),
                fail: false,
                hang: false,
            })),
            "plug-off" => SourceLookup::Unavailable(OFF_TEXT.to_owned()),
            _ => SourceLookup::Unknown,
        }
    }
}

fn with_source() -> (Pipeline, Arc<Mutex<Vec<ConnectionConfig>>>) {
    let configs = Arc::new(Mutex::new(Vec::new()));
    let mut registry = DriverRegistry::new();
    registry.add_source(Arc::new(FakeSource {
        configs: Arc::clone(&configs),
        closed: Arc::new(AtomicUsize::new(0)),
    }));
    let pipeline = Pipeline::new(
        Arc::new(registry),
        Arc::new(MemoryStore::new()) as Arc<dyn SecretStore>,
        Arc::new(FakeTunnels::new(40_001)) as Arc<dyn TunnelOpener>,
        None,
        Handle::current(),
    );
    (pipeline, configs)
}

#[tokio::test]
async fn driver_ids_include_the_drivers_of_sources() {
    let (pipeline, _) = with_source();
    assert_eq!(
        pipeline.driver_ids(),
        vec![DriverId::new("plug-ok").unwrap()]
    );
}

#[tokio::test]
async fn a_source_driver_connects_through_the_pipeline() {
    let (pipeline, configs) = with_source();
    let spec = ConnectionSpec::new(
        "Plug",
        DriverId::new("plug-ok").unwrap(),
        "postgres://u@h/db",
    );
    pipeline
        .connect(request(spec, value("secret"), SecretSource::None))
        .await
        .unwrap();
    let configs = configs.lock().unwrap();
    assert_eq!(configs.len(), 1);
    assert_eq!(configs[0].driver.as_str(), "plug-ok");
    // The pipeline put the password into the target, as for a built-in driver.
    assert_eq!(configs[0].target, "postgres://u:secret@h/db");
}

#[tokio::test]
async fn an_unavailable_source_driver_is_the_connect_failure() {
    let (pipeline, configs) = with_source();
    let spec = ConnectionSpec::new(
        "Plug",
        DriverId::new("plug-off").unwrap(),
        "postgres://u@h/db",
    );
    let Err(failure) = pipeline
        .connect(request(spec, SecretSource::None, SecretSource::None))
        .await
    else {
        panic!("expected a failure");
    };
    assert!(
        matches!(&failure, ConnectFailure::Driver(DriverError::Unavailable(t)) if t == OFF_TEXT),
        "{failure:?}"
    );
    assert!(failure.to_string().contains(OFF_TEXT));
    assert!(configs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn plain_target_goes_through_unchanged_with_config_mapping() {
    let rig = Rig::ok();
    let spec = spec("postgres://db/app?user=me&sslmode=require")
        .with_color("#aabbcc")
        .with_environment(Environment::Prod)
        .with_tls(TlsProfile::new(ProfileTlsMode::VerifyCa));
    connect(&rig, request(spec, SecretSource::None, SecretSource::None))
        .await
        .unwrap();
    assert_eq!(rig.tunnels.opened(), 0);
    let configs = rig.configs.lock().unwrap();
    let config = &configs[0];
    assert_eq!(config.target, "postgres://me@db/app?sslmode=require");
    assert_eq!(config.name, "Prod DB");
    assert_eq!(config.color.as_deref(), Some("#aabbcc"));
    assert_eq!(config.environment, Some(Environment::Prod));
    assert_eq!(config.tls.as_ref().map(|t| t.mode), Some(TlsMode::VerifyCa));
}

#[tokio::test]
async fn passwordless_url_targets_get_the_query_user_in_the_userinfo() {
    for (target, expected) in [
        ("mysql://db/app?user=app", "mysql://app@db/app"),
        (
            "mariadb://db:3307/app?user=a%40b&x=1",
            "mariadb://a%40b@db:3307/app?x=1",
        ),
        ("postgres://db/app?user=me", "postgres://me@db/app"),
        ("mysql://db/app?x=1", "mysql://db/app?x=1"),
        ("mysql://db/app", "mysql://db/app"),
    ] {
        let rig = Rig::ok();
        connect(
            &rig,
            request(spec(target), SecretSource::None, SecretSource::None),
        )
        .await
        .unwrap();
        assert_eq!(rig.targets(), [expected]);
    }
}

#[tokio::test]
async fn tunnelled_passwordless_target_rewrites_host_and_moves_the_user() {
    let rig = Rig::ok();
    let spec = spec("mysql://db.internal/app?user=app").with_ssh(ssh(SshAuth::Agent));
    connect(&rig, request(spec, SecretSource::None, SecretSource::None))
        .await
        .unwrap();
    assert_eq!(rig.targets(), ["mysql://app@127.0.0.1:40001/app"]);
}

#[tokio::test]
async fn non_url_targets_connect_without_a_password() {
    let rig = Rig::ok();
    let spec = ConnectionSpec::new("file", driver(), "/tmp/data.db");
    connect(&rig, request(spec, SecretSource::None, SecretSource::None))
        .await
        .unwrap();
    assert_eq!(rig.targets(), ["/tmp/data.db"]);
}

#[tokio::test]
async fn keychain_password_is_injected_encoded_and_user_moves_to_userinfo() {
    let rig = Rig::ok();
    let r = reference(SecretKind::DatabasePassword);
    rig.store.set(&r, &Secret::new(PASSWORD)).unwrap();
    let spec = spec("postgres://db/app?user=me").with_password_in_keychain(true);
    connect(
        &rig,
        request(spec, SecretSource::Keychain(r), SecretSource::None),
    )
    .await
    .unwrap();
    assert_eq!(rig.targets(), [format!("postgres://me:{ENCODED}@db/app")]);
}

#[tokio::test]
async fn typed_password_value_is_used_without_the_store() {
    let rig = Rig::ok();
    let spec = spec("postgres://db/app?user=me");
    connect(&rig, request(spec, value("pw"), SecretSource::None))
        .await
        .unwrap();
    assert_eq!(rig.targets(), ["postgres://me:pw@db/app"]);
}

#[tokio::test]
async fn password_without_user_is_a_target_error_that_hides_the_password() {
    let rig = Rig::ok();
    let failure = fail(
        &rig,
        request(
            spec("postgres://db/app"),
            value("hunter2"),
            SecretSource::None,
        ),
    )
    .await;
    assert!(matches!(failure, ConnectFailure::Target(_)));
    assert!(failure.to_string().contains("?user=name"));
    assert!(!failure.to_string().contains("hunter2"));
    assert_eq!(rig.targets().len(), 0);
}

#[tokio::test]
async fn password_for_a_non_url_target_is_refused() {
    let rig = Rig::ok();
    let spec = ConnectionSpec::new("file", driver(), "/tmp/data.db");
    let failure = fail(&rig, request(spec, value("pw"), SecretSource::None)).await;
    assert!(matches!(failure, ConnectFailure::Target(_)));
    assert!(!failure.to_string().contains("/tmp/data.db"));
}

#[tokio::test]
async fn missing_keychain_entries_name_the_kind() {
    let rig = Rig::ok();
    let db = reference(SecretKind::DatabasePassword);
    let failure = fail(
        &rig,
        request(
            spec("postgres://db/a?user=u"),
            SecretSource::Keychain(db),
            SecretSource::None,
        ),
    )
    .await;
    assert!(matches!(
        failure,
        ConnectFailure::MissingSecret {
            kind: SecretKind::DatabasePassword
        }
    ));
    let sshref = reference(SecretKind::SshPassword);
    let spec = spec("postgres://db/a?user=u").with_ssh(ssh(SshAuth::Password));
    let failure = fail(
        &rig,
        request(spec, SecretSource::None, SecretSource::Keychain(sshref)),
    )
    .await;
    assert!(matches!(
        failure,
        ConnectFailure::MissingSecret {
            kind: SecretKind::SshPassword
        }
    ));
    assert_eq!(rig.tunnels.opened(), 0);
}

#[tokio::test]
async fn ssh_password_auth_without_a_secret_is_missing_secret() {
    let rig = Rig::ok();
    let spec = spec("postgres://db/a?user=u").with_ssh(ssh(SshAuth::Password));
    let failure = fail(&rig, request(spec, SecretSource::None, SecretSource::None)).await;
    assert!(matches!(failure, ConnectFailure::MissingSecret { .. }));
    assert_eq!(rig.tunnels.opened(), 0);
}

struct BrokenStore;

impl SecretStore for BrokenStore {
    fn set(&self, _: &SecretRef, _: &Secret) -> Result<(), sqler_config::SecretError> {
        unreachable!()
    }

    fn get(&self, r: &SecretRef) -> Result<Option<Secret>, sqler_config::SecretError> {
        Err(sqler_config::SecretError::Unavailable {
            reference: r.clone(),
            reason: "locked".into(),
        })
    }

    fn delete(&self, _: &SecretRef) -> Result<bool, sqler_config::SecretError> {
        unreachable!()
    }
}

#[tokio::test]
async fn store_failure_is_a_secret_failure() {
    let mut rig = Rig::ok();
    rig.pipeline.secrets = Arc::new(BrokenStore);
    let r = reference(SecretKind::DatabasePassword);
    let failure = fail(
        &rig,
        request(
            spec("postgres://db/a?user=u"),
            SecretSource::Keychain(r),
            SecretSource::None,
        ),
    )
    .await;
    assert!(matches!(failure, ConnectFailure::Secret(_)));
}

#[tokio::test]
async fn tunnel_rewrites_the_target_and_closes_with_the_session() {
    let rig = Rig::ok();
    let spec = spec("postgres://db.internal:6432/app?user=me").with_ssh(ssh(SshAuth::Password));
    let session = connect(&rig, request(spec, value("dbpw"), value("sshpw")))
        .await
        .unwrap();
    assert_eq!(rig.targets(), ["postgres://me:dbpw@127.0.0.1:40001/app"]);
    {
        let opened = rig.tunnels.opened.lock().unwrap();
        let (config, auth) = &opened[0];
        assert_eq!(
            (
                config.ssh_host.as_str(),
                config.ssh_port,
                config.user.as_str()
            ),
            ("bastion.example", 2222, "deploy")
        );
        assert_eq!(
            (config.target_host.as_str(), config.target_port),
            ("db.internal", 6432)
        );
        assert_eq!(config.known_hosts, PathBuf::from("known_hosts"));
        assert!(matches!(auth, TunnelAuth::Password(s) if s.expose() == "sshpw"));
    }
    assert_eq!(rig.tunnels.closed(), 0);
    session.close().await.unwrap();
    assert_eq!(rig.tunnels.closed(), 1);
    assert_eq!(rig.session_closed.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn tunnel_without_a_password_keeps_the_rest_of_the_target() {
    let rig = Rig::ok();
    let spec = spec("postgres://db/app?user=me&sslmode=require").with_ssh(ssh(SshAuth::Agent));
    connect(&rig, request(spec, SecretSource::None, value("ignored")))
        .await
        .unwrap();
    assert_eq!(
        rig.targets(),
        ["postgres://me@127.0.0.1:40001/app?sslmode=require"]
    );
    let opened = rig.tunnels.opened.lock().unwrap();
    assert!(matches!(opened[0].1, TunnelAuth::Agent { socket: None }));
    assert_eq!(opened[0].0.ssh_port, 2222);
}

#[tokio::test]
async fn key_file_auth_carries_the_passphrase() {
    let rig = Rig::ok();
    let auth = SshAuth::KeyFile {
        path: PathBuf::from("/keys/id"),
        passphrase_in_keychain: true,
    };
    let spec = spec("postgres://db/app").with_ssh(ssh(auth));
    connect(&rig, request(spec, SecretSource::None, value("phrase")))
        .await
        .unwrap();
    let opened = rig.tunnels.opened.lock().unwrap();
    let TunnelAuth::KeyFile { path, passphrase } = &opened[0].1 else {
        panic!("expected key file auth");
    };
    assert_eq!(path, &PathBuf::from("/keys/id"));
    assert_eq!(
        passphrase.as_ref().map(TunnelSecret::expose),
        Some("phrase")
    );
}

fn verify_full_specs() -> Vec<ConnectionSpec> {
    let explicit = spec("postgres://db.internal/app?user=u")
        .with_tls(TlsProfile::new(ProfileTlsMode::VerifyFull))
        .with_ssh(ssh(SshAuth::Agent));
    let from_url =
        spec("postgres://db.internal/app?user=u&sslmode=verify-full&sslrootcert=/ca.pem")
            .with_ssh(ssh(SshAuth::Agent));
    let mysql = ConnectionSpec::new(
        "m",
        driver(),
        "mysql://db.internal/app?ssl-mode=VERIFY_IDENTITY&ssl-ca=%2Fca.pem",
    )
    .with_ssh(ssh(SshAuth::Agent));
    let alias_pg = spec("postgres://db.internal/app?user=u&ssl-mode=verify-full")
        .with_ssh(ssh(SshAuth::Agent));
    let alias_my = ConnectionSpec::new(
        "m",
        driver(),
        "mysql://db.internal/app?sslmode=VERIFY_IDENTITY",
    )
    .with_ssh(ssh(SshAuth::Agent));
    vec![explicit, from_url, mysql, alias_pg, alias_my]
}

fn tls_of(rig: &Rig) -> Vec<Option<sqler_driver_api::sqler_core::TlsConfig>> {
    rig.configs
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .map(|c| c.tls.clone())
        .collect()
}

#[tokio::test]
async fn verify_full_through_a_tunnel_maps_the_server_name_or_is_refused() {
    let rig = Rig::ok();
    for spec in verify_full_specs() {
        let result = connect(&rig, request(spec, SecretSource::None, SecretSource::None)).await;
        if cfg!(unix) {
            result.unwrap();
        } else {
            let failure = result.err().unwrap();
            assert!(
                matches!(&failure, ConnectFailure::TlsHostnameThroughTunnel { host } if host == "db.internal"),
                "{failure:?}"
            );
        }
    }
    if cfg!(unix) {
        assert_eq!(rig.tunnels.opened(), 5);
        let tls = tls_of(&rig);
        assert_eq!(tls.len(), 5);
        for tls in &tls {
            let tls = tls.as_ref().unwrap();
            assert_eq!(tls.mode, TlsMode::VerifyFull);
            assert_eq!(tls.server_name.as_deref(), Some("db.internal"));
        }
        // The URL-only forms lift the CA file out of the query.
        assert_eq!(tls[1].as_ref().unwrap().ca_cert, Some("/ca.pem".into()));
        assert_eq!(tls[2].as_ref().unwrap().ca_cert, Some("/ca.pem".into()));
        assert_eq!(tls[0].as_ref().unwrap().ca_cert, None);
    } else {
        assert_eq!(rig.tunnels.opened(), 0);
        assert_eq!(rig.targets().len(), 0);
    }
}

#[tokio::test]
async fn a_tunnelled_profile_config_gets_the_server_name_in_any_mode() {
    let rig = Rig::ok();
    let spec = spec("postgres://db.internal/app?user=u")
        .with_tls(TlsProfile::new(ProfileTlsMode::VerifyCa))
        .with_ssh(ssh(SshAuth::Agent));
    connect(&rig, request(spec, SecretSource::None, SecretSource::None))
        .await
        .unwrap();
    let tls = tls_of(&rig).remove(0).unwrap();
    assert_eq!(tls.mode, TlsMode::VerifyCa);
    assert_eq!(tls.server_name.as_deref(), Some("db.internal"));
}

#[tokio::test]
async fn a_tunnelled_target_without_tls_or_verify_full_keeps_no_tls_config() {
    let rig = Rig::ok();
    let spec =
        spec("postgres://db.internal/app?user=u&sslmode=require").with_ssh(ssh(SshAuth::Agent));
    connect(&rig, request(spec, SecretSource::None, SecretSource::None))
        .await
        .unwrap();
    assert_eq!(tls_of(&rig), [None]);
}

#[tokio::test]
async fn a_direct_connection_has_no_server_name() {
    let rig = Rig::ok();
    let plain = spec("postgres://db/app?user=u&sslmode=verify-full");
    let profile =
        spec("postgres://db/app?user=u").with_tls(TlsProfile::new(ProfileTlsMode::VerifyFull));
    for spec in [plain, profile] {
        connect(&rig, request(spec, SecretSource::None, SecretSource::None))
            .await
            .unwrap();
    }
    let tls = tls_of(&rig);
    assert_eq!(tls[0], None);
    assert_eq!(tls[1].as_ref().unwrap().server_name, None);
}

#[cfg(unix)]
#[tokio::test]
async fn a_failing_driver_with_verify_full_fails_closed_and_closes_the_tunnel() {
    let rig = Rig::new(true, false, Some(PathBuf::from("kh")));
    let spec =
        spec("postgres://db.internal/app?user=u&sslmode=verify-full").with_ssh(ssh(SshAuth::Agent));
    let failure = fail(&rig, request(spec, SecretSource::None, SecretSource::None)).await;
    assert!(matches!(failure, ConnectFailure::Driver(_)), "{failure:?}");
    assert_eq!(rig.tunnels.closed(), 1);
}

#[tokio::test]
async fn explicit_tls_overrides_a_verify_full_url_parameter() {
    let rig = Rig::ok();
    let spec = spec("postgres://db/app?sslmode=verify-full")
        .with_tls(TlsProfile::new(ProfileTlsMode::VerifyCa))
        .with_ssh(ssh(SshAuth::Agent));
    connect(&rig, request(spec, SecretSource::None, SecretSource::None))
        .await
        .unwrap();
    assert_eq!(rig.tunnels.opened(), 1);
}

#[tokio::test]
async fn verify_full_without_a_tunnel_is_fine() {
    let rig = Rig::ok();
    let spec = spec("postgres://db/app?user=u&sslmode=verify-full");
    connect(&rig, request(spec, SecretSource::None, SecretSource::None))
        .await
        .unwrap();
}

#[tokio::test]
async fn driver_failure_closes_the_tunnel() {
    let rig = Rig::new(true, false, Some(PathBuf::from("kh")));
    let spec = spec("postgres://db/app").with_ssh(ssh(SshAuth::Agent));
    let failure = fail(&rig, request(spec, SecretSource::None, SecretSource::None)).await;
    assert!(matches!(failure, ConnectFailure::Driver(_)));
    assert_eq!(rig.tunnels.closed(), 1);
}

#[tokio::test]
async fn password_without_user_closes_the_opened_tunnel() {
    let rig = Rig::ok();
    let spec = spec("postgres://db/app").with_ssh(ssh(SshAuth::Agent));
    let failure = fail(&rig, request(spec, value("pw"), SecretSource::None)).await;
    assert!(matches!(failure, ConnectFailure::Target(_)));
    assert_eq!(rig.tunnels.closed(), 1);
    assert_eq!(rig.targets().len(), 0);
}

#[tokio::test]
async fn dropping_the_attempt_closes_the_tunnel() {
    let rig = Rig::new(false, true, Some(PathBuf::from("kh")));
    let spec = spec("postgres://db/app").with_ssh(ssh(SshAuth::Agent));
    let attempt = tokio::spawn(rig.pipeline.connect(request(
        spec,
        SecretSource::None,
        SecretSource::None,
    )));
    wait_for_len(&rig).await;
    attempt.abort();
    let _ = attempt.await;
    wait_for(&rig.tunnels.closed, 1).await;
    assert_eq!(rig.tunnels.closed(), 1);
}

async fn wait_for_len(rig: &Rig) {
    for _ in 0..100 {
        if !rig.targets().is_empty() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn tunnel_errors_map_to_failures() {
    let spec = || spec("postgres://db/app").with_ssh(ssh(SshAuth::Agent));
    let rig = Rig::ok();
    *rig.tunnels.fail.lock().unwrap() = Some(TunnelError::AuthFailed);
    let failure = fail(
        &rig,
        request(spec(), SecretSource::None, SecretSource::None),
    )
    .await;
    assert_eq!(
        failure.to_string(),
        "SSH tunnel failed: SSH authentication failed"
    );
    assert_eq!(rig.targets().len(), 0);

    let key = sqler_tunnel::HostKeyInfo {
        host: "bastion.example".into(),
        port: 2222,
        algorithm: "ssh-ed25519".into(),
        fingerprint: "SHA256:abc".into(),
        public_key: "ssh-ed25519 AAAA".into(),
    };
    *rig.tunnels.fail.lock().unwrap() = Some(TunnelError::UnknownHostKey(key.clone()));
    let failure = fail(
        &rig,
        request(spec(), SecretSource::None, SecretSource::None),
    )
    .await;
    let ConnectFailure::UnknownHostKey(prompt) = failure else {
        panic!("expected UnknownHostKey");
    };
    assert_eq!(prompt.fingerprint, "SHA256:abc");

    *rig.tunnels.fail.lock().unwrap() = Some(TunnelError::HostKeyChanged { key, line: 3 });
    let failure = fail(
        &rig,
        request(spec(), SecretSource::None, SecretSource::None),
    )
    .await;
    assert!(matches!(
        failure,
        ConnectFailure::HostKeyChanged { line: 3, .. }
    ));
}

#[tokio::test]
async fn ssh_needs_a_url_target() {
    let rig = Rig::ok();
    let spec = ConnectionSpec::new("f", driver(), "/tmp/x.db").with_ssh(ssh(SshAuth::Agent));
    let failure = fail(&rig, request(spec, SecretSource::None, SecretSource::None)).await;
    assert!(matches!(failure, ConnectFailure::Target(_)));
    assert_eq!(rig.tunnels.opened(), 0);
}

#[tokio::test]
async fn missing_known_hosts_path_is_a_tunnel_failure() {
    let rig = Rig::new(false, false, None);
    let spec = spec("postgres://db/app").with_ssh(ssh(SshAuth::Agent));
    let failure = fail(&rig, request(spec, SecretSource::None, SecretSource::None)).await;
    assert_eq!(
        failure.to_string(),
        "SSH tunnel failed: no home directory for known_hosts"
    );
    assert_eq!(rig.tunnels.opened(), 0);
    let key = HostKeyPrompt {
        host: "h".into(),
        port: 22,
        algorithm: "ssh-ed25519".into(),
        fingerprint: "f".into(),
        public_key: "k".into(),
    };
    assert!(matches!(
        rig.pipeline.trust_host_key(key).await,
        Err(ConnectFailure::Tunnel(_))
    ));
}

#[tokio::test]
async fn trust_host_key_writes_known_hosts() {
    let dir = std::env::temp_dir().join(format!("sqler-app-trust-{}", std::process::id()));
    let path = dir.join("known_hosts");
    let rig = Rig::new(false, false, Some(path.clone()));
    let key = HostKeyPrompt {
        host: "bastion.example".into(),
        port: 2222,
        algorithm: "ssh-ed25519".into(),
        fingerprint: "SHA256:x".into(),
        public_key:
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl"
                .into(),
    };
    rig.pipeline.trust_host_key(key).await.unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(
        text.starts_with("[bastion.example]:2222 ssh-ed25519 AAAA"),
        "{text}"
    );

    let bad = HostKeyPrompt {
        host: "h".into(),
        port: 22,
        algorithm: "ssh-ed25519".into(),
        fingerprint: "f".into(),
        public_key: "garbage".into(),
    };
    let failure = rig.pipeline.trust_host_key(bad).await.unwrap_err();
    assert!(matches!(failure, ConnectFailure::Tunnel(_)));
    std::fs::remove_dir_all(&dir).unwrap();
}
