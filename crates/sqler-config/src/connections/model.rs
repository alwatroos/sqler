// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Domain types of the connection store: ids, specs, TLS and SSH profiles.

use std::fmt;
use std::path::PathBuf;

use sqler_core::{ConnectionConfig, DriverId, Environment, TlsConfig, TlsMode as CoreTlsMode};

use super::store::StoreError;
use crate::secrets::{MAX_ID_LEN, SecretError, SecretRef};

/// Highest version of the `connections.toml` format this build reads and writes. A save writes
/// version 1 unless a connection has AI settings (`[connection.ai]`, ADR-0035), so older builds
/// keep working until AI is used.
pub const CONNECTIONS_FORMAT_VERSION: u32 = 2;

/// Whether `s` is a valid id: lowercase letters, digits and `-` only, and
/// within the keychain connection-id grammar (1 to 64 characters, leading
/// letter or digit), so every id can name a keychain entry.
pub(crate) fn is_valid_id(s: &str) -> bool {
    id_problem(s).is_none()
}

/// Why `s` is not a valid id, `None` when it is valid.
pub(crate) fn id_problem(s: &str) -> Option<String> {
    if let Err(reason) = crate::secrets::validate_id(s) {
        return Some(reason.to_owned());
    }
    if !s
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Some("id may only contain lowercase letters, digits and '-'".to_owned());
    }
    None
}

/// `slug` cut to at most `max` bytes (it is ASCII), without a trailing `-`.
fn cap(slug: &str, max: usize) -> &str {
    slug[..slug.len().min(max)].trim_end_matches('-')
}

/// Lowercases `name` into a slug, collapsing runs of other characters into one
/// `-`, then makes it unique with `-2`, `-3`, ... according to `taken`.
pub(crate) fn unique_slug(name: &str, fallback: &str, taken: impl Fn(&str) -> bool) -> String {
    let mut slug = String::new();
    let mut pending_dash = false;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            if pending_dash && !slug.is_empty() {
                slug.push('-');
            }
            pending_dash = false;
            slug.push(c.to_ascii_lowercase());
        } else {
            pending_dash = true;
        }
    }
    if slug.is_empty() {
        fallback.clone_into(&mut slug);
    }
    let slug = cap(&slug, MAX_ID_LEN).to_owned();
    if !taken(&slug) {
        return slug;
    }
    let mut n = 2_u64;
    loop {
        let suffix = format!("-{n}");
        let candidate = format!("{}{suffix}", cap(&slug, MAX_ID_LEN - suffix.len()));
        if !taken(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

macro_rules! id_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(String);

        impl $name {
            /// The id as a string slice.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// Wraps an id already known to be valid.
            pub(crate) fn from_valid(id: String) -> Self {
                debug_assert!(is_valid_id(&id));
                Self(id)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

id_type!(
    /// Stable identifier of a saved connection: `[a-z0-9-]+`, derived from the
    /// name at creation and never changed afterwards.
    ConnectionId
);
id_type!(
    /// Stable identifier of a group: `[a-z0-9-]+`, derived from the name at
    /// creation and never changed afterwards.
    GroupId
);

/// Which secret of a connection a keychain entry holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum ProfileSecretKind {
    /// The database password.
    DbPassword,
    /// The SSH account password.
    SshPassword,
    /// The passphrase of the SSH key file.
    SshPassphrase,
}

impl ProfileSecretKind {
    /// The identifier used in keychain account names.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DbPassword => "db-password",
            Self::SshPassword => "ssh-password",
            Self::SshPassphrase => "ssh-key-passphrase",
        }
    }
}

/// A reference to a secret held in the OS keychain; never the secret itself.
///
/// Derived from the stable [`ConnectionId`], so renaming a connection keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProfileSecret {
    connection: ConnectionId,
    kind: ProfileSecretKind,
}

impl ProfileSecret {
    pub(crate) fn new(connection: ConnectionId, kind: ProfileSecretKind) -> Self {
        Self { connection, kind }
    }

    /// The connection the secret belongs to.
    #[must_use]
    pub fn connection(&self) -> &ConnectionId {
        &self.connection
    }

    /// Which secret of the connection this is.
    #[must_use]
    pub fn kind(&self) -> ProfileSecretKind {
        self.kind
    }

    /// The keychain account name, `<connection id>:<kind>`; the string form of
    /// the matching [`SecretRef`].
    #[must_use]
    pub fn account(&self) -> String {
        format!("{}:{}", self.connection, self.kind.as_str())
    }
}

impl From<ProfileSecretKind> for crate::secrets::SecretKind {
    fn from(kind: ProfileSecretKind) -> Self {
        match kind {
            ProfileSecretKind::DbPassword => Self::DatabasePassword,
            ProfileSecretKind::SshPassword => Self::SshPassword,
            ProfileSecretKind::SshPassphrase => Self::SshKeyPassphrase,
        }
    }
}

impl TryFrom<&ProfileSecret> for SecretRef {
    type Error = SecretError;

    /// Fails only if the connection id is longer than 64 characters or starts
    /// with `-`, which the keychain reference grammar does not allow.
    fn try_from(secret: &ProfileSecret) -> Result<Self, Self::Error> {
        SecretRef::new(secret.connection.as_str(), secret.kind.into())
    }
}

/// How strictly to use TLS towards the database.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TlsMode {
    /// No TLS.
    Disable,
    /// Use TLS when the server offers it.
    Prefer,
    /// Require TLS without verifying the certificate.
    Require,
    /// Require TLS and verify the server certificate chain.
    VerifyCa,
    /// Require TLS and verify the chain and the host name.
    VerifyFull,
}

impl TlsMode {
    /// The kebab-case identifier used in the file.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disable => "disable",
            Self::Prefer => "prefer",
            Self::Require => "require",
            Self::VerifyCa => "verify-ca",
            Self::VerifyFull => "verify-full",
        }
    }

    fn to_core(self) -> CoreTlsMode {
        match self {
            Self::Disable => CoreTlsMode::Disable,
            Self::Prefer => CoreTlsMode::Prefer,
            Self::Require => CoreTlsMode::Require,
            Self::VerifyCa => CoreTlsMode::VerifyCa,
            Self::VerifyFull => CoreTlsMode::VerifyFull,
        }
    }

    /// Parses an identifier produced by [`TlsMode::as_str`] (exact match).
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "disable" => Some(Self::Disable),
            "prefer" => Some(Self::Prefer),
            "require" => Some(Self::Require),
            "verify-ca" => Some(Self::VerifyCa),
            "verify-full" => Some(Self::VerifyFull),
            _ => None,
        }
    }
}

/// TLS settings of a connection; see ADR-0011.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct TlsProfile {
    /// How strictly to use TLS.
    pub mode: TlsMode,
    /// Certificate authority bundle to verify the server with.
    pub ca_cert: Option<PathBuf>,
    /// Client certificate; set together with `client_key`.
    pub client_cert: Option<PathBuf>,
    /// Client private key file; set together with `client_cert`.
    pub client_key: Option<PathBuf>,
}

impl TlsProfile {
    /// A TLS profile with the given mode and no certificate files.
    #[must_use]
    pub fn new(mode: TlsMode) -> Self {
        Self {
            mode,
            ca_cert: None,
            client_cert: None,
            client_key: None,
        }
    }

    /// The `sqler-core` TLS options for this profile.
    #[must_use]
    pub fn to_core(&self) -> TlsConfig {
        let mut cfg = TlsConfig::new(self.mode.to_core());
        if let Some(p) = &self.ca_cert {
            cfg = cfg.with_ca_cert(p.clone());
        }
        if let Some(p) = &self.client_cert {
            cfg = cfg.with_client_cert(p.clone());
        }
        if let Some(p) = &self.client_key {
            cfg = cfg.with_client_key(p.clone());
        }
        cfg
    }
}

/// How to authenticate to the SSH server.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SshAuth {
    /// Account password, kept in the keychain.
    Password,
    /// A private key file.
    KeyFile {
        /// Path of the private key.
        path: PathBuf,
        /// Whether the key's passphrase is kept in the keychain.
        passphrase_in_keychain: bool,
    },
    /// The running SSH agent.
    Agent,
}

/// An SSH tunnel through which the database is reached.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SshProfile {
    /// SSH server host.
    pub host: String,
    /// SSH server port.
    pub port: u16,
    /// SSH user name.
    pub user: String,
    /// How to authenticate.
    pub auth: SshAuth,
}

impl SshProfile {
    /// An SSH profile on port 22.
    #[must_use]
    pub fn new(host: impl Into<String>, user: impl Into<String>, auth: SshAuth) -> Self {
        Self {
            host: host.into(),
            port: 22,
            user: user.into(),
            auth,
        }
    }
}

/// Per-connection AI settings (ADR-0035): opt-in, and whether result rows may be sent.
///
/// The default is off. It is stored as the optional `[connection.ai]` table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct ConnectionAi {
    /// Whether AI features may be used with this connection.
    pub enabled: bool,
    /// Whether result rows may be sent to the provider; `None` means the environment default
    /// (on, except for `prod`).
    pub send_rows: Option<bool>,
}

impl ConnectionAi {
    /// Settings with AI on or off and the default for sending rows.
    #[must_use]
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            send_rows: None,
        }
    }

    /// Sets the explicit "send row data" choice.
    #[must_use]
    pub fn with_send_rows(mut self, yes: bool) -> Self {
        self.send_rows = Some(yes);
        self
    }

    /// Whether these are the defaults (AI off, no explicit choice); defaults are not written.
    #[must_use]
    pub fn is_default(&self) -> bool {
        !self.enabled && self.send_rows.is_none()
    }

    /// Whether result rows may be sent: AI is on and the explicit choice, or else on for every
    /// environment except `prod`.
    #[must_use]
    pub fn sends_rows(&self, env: Option<Environment>) -> bool {
        self.enabled && self.send_rows.unwrap_or(env != Some(Environment::Prod))
    }
}

/// Everything the user edits about one connection (no id, no group).
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ConnectionSpec {
    /// Human-readable name, unique among all connections.
    pub name: String,
    /// The driver that handles this connection.
    pub driver: DriverId,
    /// Driver-specific target; must not embed a secret.
    pub target: String,
    /// Accent colour as `#rrggbb`.
    pub color: Option<String>,
    /// Environment tag.
    pub environment: Option<Environment>,
    /// TLS settings.
    pub tls: Option<TlsProfile>,
    /// SSH tunnel.
    pub ssh: Option<SshProfile>,
    /// Whether the database password is kept in the keychain.
    pub password_in_keychain: bool,
    /// AI settings; off by default.
    pub ai: ConnectionAi,
}

impl ConnectionSpec {
    /// A spec with no colour, environment, TLS, SSH or keychain password.
    #[must_use]
    pub fn new(name: impl Into<String>, driver: DriverId, target: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            driver,
            target: target.into(),
            color: None,
            environment: None,
            tls: None,
            ssh: None,
            password_in_keychain: false,
            ai: ConnectionAi::default(),
        }
    }

    /// Sets the accent colour (`#rrggbb`).
    #[must_use]
    pub fn with_color(mut self, color: impl Into<String>) -> Self {
        self.color = Some(color.into());
        self
    }

    /// Sets the environment tag.
    #[must_use]
    pub fn with_environment(mut self, env: Environment) -> Self {
        self.environment = Some(env);
        self
    }

    /// Sets the TLS profile.
    #[must_use]
    pub fn with_tls(mut self, tls: TlsProfile) -> Self {
        self.tls = Some(tls);
        self
    }

    /// Sets the SSH tunnel profile.
    #[must_use]
    pub fn with_ssh(mut self, ssh: SshProfile) -> Self {
        self.ssh = Some(ssh);
        self
    }

    /// Marks whether the database password is kept in the keychain.
    #[must_use]
    pub fn with_password_in_keychain(mut self, yes: bool) -> Self {
        self.password_in_keychain = yes;
        self
    }

    /// Sets the AI settings.
    #[must_use]
    pub fn with_ai(mut self, ai: ConnectionAi) -> Self {
        self.ai = ai;
        self
    }

    fn core_config(&self) -> ConnectionConfig {
        ConnectionConfig::new(self.name.clone(), self.driver.clone(), self.target.clone())
    }
}

impl fmt::Debug for ConnectionSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let target: &dyn fmt::Debug = if self.core_config().target_has_secret() {
            &"<redacted>"
        } else {
            &self.target
        };
        f.debug_struct("ConnectionSpec")
            .field("name", &self.name)
            .field("driver", &self.driver)
            .field("target", target)
            .field("color", &self.color)
            .field("environment", &self.environment)
            .field("tls", &self.tls)
            .field("ssh", &self.ssh)
            .field("password_in_keychain", &self.password_in_keychain)
            .field("ai", &self.ai)
            .finish()
    }
}

/// Checks and normalises a spec: trimmed name, lowercase `#rrggbb` colour, no
/// secret in the target, consistent TLS and SSH settings.
pub(crate) fn validate_spec(mut spec: ConnectionSpec) -> Result<ConnectionSpec, StoreError> {
    let name = spec.name.trim();
    if name.is_empty() {
        return Err(StoreError::EmptyName);
    }
    spec.name = name.to_owned();
    if let Some(color) = &spec.color {
        let ok = color.len() == 7
            && color.starts_with('#')
            && color[1..].chars().all(|c| c.is_ascii_hexdigit());
        if !ok {
            return Err(StoreError::InvalidColor(color.clone()));
        }
        spec.color = Some(color.to_ascii_lowercase());
    }
    if spec.core_config().target_has_secret() {
        return Err(StoreError::TargetHasSecret);
    }
    if let Some(tls) = &spec.tls
        && tls.client_cert.is_some() != tls.client_key.is_some()
    {
        return Err(StoreError::InvalidTls(
            "client_cert and client_key must be set together".to_owned(),
        ));
    }
    if let Some(ssh) = &mut spec.ssh {
        ssh.host = ssh.host.trim().to_owned();
        ssh.user = ssh.user.trim().to_owned();
        if ssh.host.is_empty() {
            return Err(StoreError::InvalidSsh("host must not be empty".to_owned()));
        }
        if ssh.user.is_empty() {
            return Err(StoreError::InvalidSsh("user must not be empty".to_owned()));
        }
        if ssh.port == 0 {
            return Err(StoreError::InvalidSsh("port must not be 0".to_owned()));
        }
        if let SshAuth::KeyFile { path, .. } = &ssh.auth
            && path.as_os_str().is_empty()
        {
            return Err(StoreError::InvalidSsh(
                "key_file must not be empty".to_owned(),
            ));
        }
    }
    Ok(spec)
}

/// A named folder holding groups and connections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    id: GroupId,
    name: String,
    parent: Option<GroupId>,
}

impl Group {
    pub(crate) fn new(id: GroupId, name: String, parent: Option<GroupId>) -> Self {
        Self { id, name, parent }
    }

    pub(crate) fn set_name(&mut self, name: String) {
        self.name = name;
    }

    pub(crate) fn set_parent(&mut self, parent: Option<GroupId>) {
        self.parent = parent;
    }

    /// The stable id.
    #[must_use]
    pub fn id(&self) -> &GroupId {
        &self.id
    }

    /// The display name, unique among siblings.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The enclosing group, `None` at the root.
    #[must_use]
    pub fn parent(&self) -> Option<&GroupId> {
        self.parent.as_ref()
    }
}

/// A saved connection: stable id, optional group and its [`ConnectionSpec`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionProfile {
    id: ConnectionId,
    group: Option<GroupId>,
    spec: ConnectionSpec,
}

impl ConnectionProfile {
    pub(crate) fn new(id: ConnectionId, group: Option<GroupId>, spec: ConnectionSpec) -> Self {
        Self { id, group, spec }
    }

    pub(crate) fn set_group(&mut self, group: Option<GroupId>) {
        self.group = group;
    }

    pub(crate) fn set_spec(&mut self, spec: ConnectionSpec) {
        self.spec = spec;
    }

    /// The stable id.
    #[must_use]
    pub fn id(&self) -> &ConnectionId {
        &self.id
    }

    /// The enclosing group, `None` at the root.
    #[must_use]
    pub fn group(&self) -> Option<&GroupId> {
        self.group.as_ref()
    }

    /// The user-editable settings.
    #[must_use]
    pub fn spec(&self) -> &ConnectionSpec {
        &self.spec
    }

    /// Whether AI features are enabled for this connection.
    #[must_use]
    pub fn ai_enabled(&self) -> bool {
        self.spec.ai.enabled
    }

    /// Whether result rows may be sent to an AI provider for this connection.
    #[must_use]
    pub fn ai_sends_rows(&self) -> bool {
        self.spec.ai.sends_rows(self.spec.environment)
    }

    /// The keychain reference of the database password, when one is kept.
    #[must_use]
    pub fn password_ref(&self) -> Option<ProfileSecret> {
        self.spec
            .password_in_keychain
            .then(|| ProfileSecret::new(self.id.clone(), ProfileSecretKind::DbPassword))
    }

    /// The keychain reference of the SSH password or key passphrase, when one is kept.
    #[must_use]
    pub fn ssh_secret_ref(&self) -> Option<ProfileSecret> {
        let kind = match &self.spec.ssh.as_ref()?.auth {
            SshAuth::Password => ProfileSecretKind::SshPassword,
            SshAuth::KeyFile {
                passphrase_in_keychain: true,
                ..
            } => ProfileSecretKind::SshPassphrase,
            _ => return None,
        };
        Some(ProfileSecret::new(self.id.clone(), kind))
    }

    /// Every keychain reference of this profile; delete these with the profile.
    #[must_use]
    pub fn secret_refs(&self) -> Vec<ProfileSecret> {
        self.password_ref()
            .into_iter()
            .chain(self.ssh_secret_ref())
            .collect()
    }

    /// The `sqler-core` configuration for this profile (the SSH tunnel is not part of it).
    #[must_use]
    pub fn to_connection_config(&self) -> ConnectionConfig {
        let mut cfg = self.spec.core_config();
        if let Some(color) = &self.spec.color {
            cfg = cfg.with_color(color.clone());
        }
        if let Some(env) = self.spec.environment {
            cfg = cfg.with_environment(env);
        }
        if let Some(tls) = &self.spec.tls {
            cfg = cfg.with_tls(tls.to_core());
        }
        cfg
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sqlite() -> DriverId {
        DriverId::new("sqlite").unwrap()
    }

    #[test]
    fn slug_rules() {
        let none = |_: &str| false;
        assert_eq!(
            unique_slug("My DB (prod)!", "connection", none),
            "my-db-prod"
        );
        assert_eq!(unique_slug("  --A__b  ", "connection", none), "a-b");
        assert_eq!(unique_slug("日本", "group", none), "group");
        let taken = |s: &str| s == "a" || s == "a-2";
        assert_eq!(unique_slug("A", "connection", taken), "a-3");
    }

    #[test]
    fn long_names_yield_keychain_safe_ids() {
        let name = "x".repeat(70);
        let id = unique_slug(&name, "connection", |_| false);
        assert_eq!(id.len(), 64);
        assert!(is_valid_id(&id));
        let profile = ProfileSecret::new(
            ConnectionId::from_valid(id.clone()),
            ProfileSecretKind::DbPassword,
        );
        assert!(SecretRef::try_from(&profile).is_ok());

        let mut taken = std::collections::HashSet::new();
        for _ in 0..12 {
            let next = unique_slug(&name, "connection", |s| taken.contains(s));
            assert!(next.len() <= 64 && is_valid_id(&next), "{next}");
            assert!(taken.insert(next));
        }
        // A cut that lands on a '-' does not leave a trailing dash.
        let dashed = format!("{}-{}", "a".repeat(63), "b");
        let id = unique_slug(&dashed, "connection", |_| false);
        assert!(!id.ends_with('-') && is_valid_id(&id), "{id}");
    }

    #[test]
    fn id_validity_matches_keychain_grammar() {
        let cases: Vec<String> = ["a", "-x", "x-", "", "a.b", "A"]
            .into_iter()
            .map(str::to_owned)
            .chain(["a".repeat(64), "a".repeat(65)])
            .collect();
        for id in &cases {
            let keychain_ok =
                SecretRef::new(id, crate::secrets::SecretKind::DatabasePassword).is_ok();
            if is_valid_id(id) {
                assert!(keychain_ok, "{id:?} valid but not a keychain id");
            }
        }
        assert!(!is_valid_id("-x") && !is_valid_id(&"a".repeat(65)) && !is_valid_id(""));
        assert!(is_valid_id(&"a".repeat(64)));
    }

    #[test]
    fn secret_account_and_kinds() {
        let s = ProfileSecret::new(
            ConnectionId::from_valid("billing".into()),
            ProfileSecretKind::SshPassphrase,
        );
        assert_eq!(s.account(), "billing:ssh-key-passphrase");
        assert_eq!(s.connection().as_str(), "billing");
        assert_eq!(s.kind(), ProfileSecretKind::SshPassphrase);
        assert_eq!(ProfileSecretKind::DbPassword.as_str(), "db-password");
        assert_eq!(ProfileSecretKind::SshPassword.as_str(), "ssh-password");
    }

    #[test]
    fn secret_converts_to_secret_ref_with_same_string() {
        for kind in [
            ProfileSecretKind::DbPassword,
            ProfileSecretKind::SshPassword,
            ProfileSecretKind::SshPassphrase,
        ] {
            let s = ProfileSecret::new(ConnectionId::from_valid("billing".into()), kind);
            let r = SecretRef::try_from(&s).unwrap();
            assert_eq!(r.to_string(), s.account());
            assert_eq!(r.to_string().parse::<SecretRef>().unwrap(), r);
            assert_eq!(r.connection_id(), "billing");
        }
        let r = SecretRef::try_from(&ProfileSecret::new(
            ConnectionId::from_valid("billing".into()),
            ProfileSecretKind::DbPassword,
        ))
        .unwrap();
        assert_eq!(r.to_string(), "billing:db-password");
        let bad = ProfileSecret::new(ConnectionId("-x".into()), ProfileSecretKind::DbPassword);
        assert!(SecretRef::try_from(&bad).is_err());
    }

    #[test]
    fn tls_mode_ids_round_trip() {
        for m in [
            TlsMode::Disable,
            TlsMode::Prefer,
            TlsMode::Require,
            TlsMode::VerifyCa,
            TlsMode::VerifyFull,
        ] {
            assert_eq!(TlsMode::from_id(m.as_str()), Some(m));
        }
        assert_eq!(TlsMode::from_id("Require"), None);
    }

    #[test]
    fn validation_rules() {
        let spec = || ConnectionSpec::new(" n ", sqlite(), "app.db");
        let ok = validate_spec(spec().with_color("#AABBCC")).unwrap();
        assert_eq!(
            (ok.name.as_str(), ok.color.as_deref()),
            ("n", Some("#aabbcc"))
        );
        assert_eq!(
            validate_spec(ConnectionSpec::new("  ", sqlite(), "x")),
            Err(StoreError::EmptyName)
        );
        assert!(matches!(
            validate_spec(spec().with_color("red")),
            Err(StoreError::InvalidColor(_))
        ));
        let pg = DriverId::new("postgres").unwrap();
        assert_eq!(
            validate_spec(ConnectionSpec::new("p", pg, "postgres://u:pw@h/d")),
            Err(StoreError::TargetHasSecret)
        );
        let mut tls = TlsProfile::new(TlsMode::Require);
        tls.client_cert = Some("c.pem".into());
        assert!(matches!(
            validate_spec(spec().with_tls(tls)),
            Err(StoreError::InvalidTls(_))
        ));
        let ssh = |h: &str, u: &str, port: u16| {
            let mut p = SshProfile::new(h, u, SshAuth::Agent);
            p.port = port;
            spec().with_ssh(p)
        };
        assert!(validate_spec(ssh("h", "u", 22)).is_ok());
        for bad in [ssh(" ", "u", 22), ssh("h", "", 22), ssh("h", "u", 0)] {
            assert!(matches!(validate_spec(bad), Err(StoreError::InvalidSsh(_))));
        }
    }

    #[test]
    fn spec_debug_redacts_secret_target() {
        let pg = DriverId::new("postgres").unwrap();
        let s = ConnectionSpec::new("p", pg, "postgres://u:hunter2@h/d");
        let d = format!("{s:?}");
        assert!(d.contains("<redacted>") && !d.contains("hunter2"), "{d}");
        let d = format!("{:?}", ConnectionSpec::new("p", sqlite(), "app.db"));
        assert!(d.contains("app.db"));
    }

    #[test]
    fn ai_defaults_off_and_is_not_written() {
        let ai = ConnectionAi::default();
        assert!(ai.is_default() && !ai.enabled && ai.send_rows.is_none());
        assert!(!ConnectionAi::new(true).is_default());
        assert!(!ConnectionAi::new(false).with_send_rows(true).is_default());
        let spec = ConnectionSpec::new("c", sqlite(), "x");
        assert!(spec.ai.is_default());
        let p = ConnectionProfile::new(ConnectionId::from_valid("c".into()), None, spec);
        assert!(!p.ai_enabled() && !p.ai_sends_rows());
    }

    #[test]
    fn sends_rows_table() {
        use Environment::{Dev, Prod, Staging};
        for env in [None, Some(Dev), Some(Staging), Some(Prod)] {
            for send in [None, Some(true), Some(false)] {
                for enabled in [false, true] {
                    let mut ai = ConnectionAi::new(enabled);
                    ai.send_rows = send;
                    let default = env != Some(Prod);
                    let want = enabled && send.unwrap_or(default);
                    assert_eq!(ai.sends_rows(env), want, "{env:?} {send:?} {enabled}");
                }
            }
        }
        let prod = ConnectionProfile::new(
            ConnectionId::from_valid("c".into()),
            None,
            ConnectionSpec::new("c", sqlite(), "x")
                .with_environment(Prod)
                .with_ai(ConnectionAi::new(true)),
        );
        assert!(prod.ai_enabled() && !prod.ai_sends_rows());
    }

    #[test]
    fn secret_refs_follow_flags() {
        let id = ConnectionId::from_valid("c".into());
        let plain =
            ConnectionProfile::new(id.clone(), None, ConnectionSpec::new("c", sqlite(), "x"));
        assert_eq!(plain.secret_refs().len(), 0);
        let spec = ConnectionSpec::new("c", sqlite(), "x")
            .with_password_in_keychain(true)
            .with_ssh(SshProfile::new("h", "u", SshAuth::Password));
        let p = ConnectionProfile::new(id.clone(), None, spec);
        let accounts: Vec<_> = p.secret_refs().iter().map(ProfileSecret::account).collect();
        assert_eq!(accounts, ["c:db-password", "c:ssh-password"]);
        let key = SshAuth::KeyFile {
            path: "k".into(),
            passphrase_in_keychain: true,
        };
        let spec = ConnectionSpec::new("c", sqlite(), "x").with_ssh(SshProfile::new("h", "u", key));
        let p = ConnectionProfile::new(id, None, spec);
        assert_eq!(
            p.ssh_secret_ref().unwrap().kind(),
            ProfileSecretKind::SshPassphrase
        );
        assert!(p.password_ref().is_none());
    }

    #[test]
    fn to_connection_config_maps_fields() {
        let spec = ConnectionSpec::new("n", sqlite(), "app.db")
            .with_color("#112233")
            .with_environment(Environment::Prod);
        let p = ConnectionProfile::new(ConnectionId::from_valid("n".into()), None, spec);
        let cfg = p.to_connection_config();
        assert_eq!(cfg.name, "n");
        assert_eq!(cfg.driver.as_str(), "sqlite");
        assert_eq!(cfg.target, "app.db");
        assert_eq!(cfg.color.as_deref(), Some("#112233"));
        assert_eq!(cfg.environment, Some(Environment::Prod));
    }

    #[test]
    fn to_connection_config_maps_tls() {
        let mut tls = TlsProfile::new(TlsMode::VerifyFull);
        tls.ca_cert = Some("/ca.pem".into());
        tls.client_cert = Some("/c.pem".into());
        tls.client_key = Some("/k.pem".into());
        let spec = ConnectionSpec::new("n", sqlite(), "x").with_tls(tls);
        let p = ConnectionProfile::new(ConnectionId::from_valid("n".into()), None, spec);
        let got = p.to_connection_config().tls.unwrap();
        let want = TlsConfig::new(CoreTlsMode::VerifyFull)
            .with_ca_cert("/ca.pem")
            .with_client_cert("/c.pem")
            .with_client_key("/k.pem");
        assert_eq!(got, want);

        for (m, c) in [
            (TlsMode::Disable, CoreTlsMode::Disable),
            (TlsMode::Prefer, CoreTlsMode::Prefer),
            (TlsMode::Require, CoreTlsMode::Require),
            (TlsMode::VerifyCa, CoreTlsMode::VerifyCa),
        ] {
            assert_eq!(TlsProfile::new(m).to_core(), TlsConfig::new(c));
        }
        let plain = ConnectionSpec::new("n", sqlite(), "x");
        let p = ConnectionProfile::new(ConnectionId::from_valid("n".into()), None, plain);
        assert_eq!(p.to_connection_config().tls, None);
    }
}
