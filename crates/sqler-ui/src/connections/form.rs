// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `ConnectionForm`: the headless state machine behind the new/edit connection dialog.
//!
//! Text fields are plain `String`s; the view owns the text inputs and reports every change
//! through [`ConnectionForm::set_text`]. Passwords never go into the spec: they are a
//! [`SecretField`] plan that the workspace turns into keychain writes.

use std::path::PathBuf;

use sqler_config::{
    ConnectionId, ConnectionProfile, ConnectionSpec, GroupId, Secret, SecretKind, SecretRef,
    SshAuth, SshProfile, StoreError, TlsMode, TlsProfile,
};
use sqler_driver_api::sqler_core::{ConnectionConfig, DriverId, Environment};

use super::connector::{ConnectRequest, SecretSource, from_profile_secret};
use crate::tabs::Rgb;
use crate::workspace::RequestId;

/// Accent colour presets offered by the form, as `#rrggbb`.
pub const COLOR_PRESETS: [&str; 8] = [
    "#e5484d", "#f76b15", "#f5d90a", "#30a46c", "#00a2c7", "#3e63dd", "#8e4ec6", "#8b8d98",
];

/// New connection, or edit of a saved one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormMode {
    /// A new connection.
    New,
    /// An existing connection; its id and keychain references stay.
    Edit(ConnectionId),
}

/// A field of the form that can carry an error or a text value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FormField {
    /// The connection name.
    Name,
    /// The target: a URL, a file path or whatever the driver reads.
    Target,
    /// The accent colour as `#rrggbb` (empty for none).
    Color,
    /// TLS certificate authority bundle path.
    CaCert,
    /// TLS client certificate path.
    ClientCert,
    /// TLS client key path.
    ClientKey,
    /// SSH server host.
    SshHost,
    /// SSH server port.
    SshPort,
    /// SSH user.
    SshUser,
    /// SSH private key file path.
    SshKeyPath,
    /// The database password (a secret, not a text value).
    DbPassword,
    /// The SSH password or key passphrase (a secret, not a text value).
    SshSecret,
}

/// Which secret of the form is meant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretSlot {
    /// The database password.
    Db,
    /// The SSH password or key passphrase.
    Ssh,
}

/// How the tunnel authenticates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SshAuthKind {
    /// Account password.
    Password,
    /// A private key file.
    KeyFile,
    /// The running SSH agent.
    Agent,
}

impl SshAuthKind {
    /// The kind of an existing auth setting.
    fn of(auth: &SshAuth) -> Self {
        match auth {
            SshAuth::KeyFile { .. } => Self::KeyFile,
            SshAuth::Agent => Self::Agent,
            _ => Self::Password,
        }
    }

    /// The secret this kind keeps in the keychain, if any.
    fn secret_kind(self) -> Option<SecretKind> {
        match self {
            Self::Password => Some(SecretKind::SshPassword),
            Self::KeyFile => Some(SecretKind::SshKeyPassphrase),
            Self::Agent => None,
        }
    }
}

/// What to do with a secret on save.
#[derive(Debug, Clone)]
pub enum SecretField {
    /// Keep the stored one (valid only where the same reference existed before).
    Keep,
    /// Store this new value.
    Set(Secret),
    /// Store none; delete an existing entry.
    Clear,
}

/// The outcome of "Test connection".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestStatus {
    /// No test ran, or it was cancelled.
    Idle,
    /// A test is running as this request.
    Running(RequestId),
    /// The last test connected.
    Ok,
    /// The last test failed with this message.
    Failed(String),
}

/// One problem found by validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldError {
    /// The field it belongs to, `None` for the form as a whole.
    pub field: Option<FormField>,
    /// What is wrong, fit to show next to the field.
    pub message: String,
}

impl FieldError {
    fn new(field: FormField, message: impl Into<String>) -> Self {
        Self {
            field: Some(field),
            message: message.into(),
        }
    }

    fn general(message: impl Into<String>) -> Self {
        Self {
            field: None,
            message: message.into(),
        }
    }

    /// Maps a store rejection to the field it concerns.
    #[must_use]
    pub fn from_store(error: &StoreError) -> Self {
        let field = match error {
            StoreError::EmptyName | StoreError::DuplicateConnectionName(_) => Some(FormField::Name),
            StoreError::InvalidColor(_) => Some(FormField::Color),
            StoreError::TargetHasSecret => Some(FormField::Target),
            StoreError::InvalidTls(_) => Some(FormField::ClientCert),
            StoreError::InvalidSsh(_) => Some(FormField::SshHost),
            _ => None,
        };
        Self {
            field,
            message: error.to_string(),
        }
    }
}

/// A form that passed validation: what to store and what to do with the secrets.
#[derive(Debug, Clone)]
pub struct ValidForm {
    /// The connection settings to store.
    pub spec: ConnectionSpec,
    /// The group the connection belongs to (`None` = root).
    pub group: Option<GroupId>,
    /// The database password plan.
    pub db_password: SecretField,
    /// The SSH password or key passphrase plan.
    pub ssh_secret: SecretField,
}

/// The new/edit connection form.
#[derive(Debug, Clone)]
pub struct ConnectionForm {
    mode: FormMode,
    original: Option<ConnectionProfile>,
    name: String,
    target: String,
    color: String,
    ca_cert: String,
    client_cert: String,
    client_key: String,
    ssh_host: String,
    ssh_port: String,
    ssh_user: String,
    ssh_key_path: String,
    driver: Option<DriverId>,
    environment: Option<Environment>,
    group: Option<GroupId>,
    tls_mode: Option<TlsMode>,
    ssh_enabled: bool,
    ssh_auth: SshAuthKind,
    db_password: SecretField,
    ssh_secret: SecretField,
    errors: Vec<FieldError>,
    /// The state of "Test connection".
    pub test: TestStatus,
}

fn path_text(path: Option<&PathBuf>) -> String {
    path.map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn optional_path(text: &str) -> Option<PathBuf> {
    let text = text.trim();
    (!text.is_empty()).then(|| PathBuf::from(text))
}

impl ConnectionForm {
    fn blank(mode: FormMode) -> Self {
        Self {
            mode,
            original: None,
            name: String::new(),
            target: String::new(),
            color: String::new(),
            ca_cert: String::new(),
            client_cert: String::new(),
            client_key: String::new(),
            ssh_host: String::new(),
            ssh_port: "22".to_owned(),
            ssh_user: String::new(),
            ssh_key_path: String::new(),
            driver: None,
            environment: None,
            group: None,
            tls_mode: None,
            ssh_enabled: false,
            ssh_auth: SshAuthKind::Password,
            db_password: SecretField::Clear,
            ssh_secret: SecretField::Clear,
            errors: Vec::new(),
            test: TestStatus::Idle,
        }
    }

    /// An empty form for a new connection in `group`, with `driver` preselected.
    #[must_use]
    pub fn new_connection(driver: Option<DriverId>, group: Option<GroupId>) -> Self {
        Self {
            driver,
            group,
            ..Self::blank(FormMode::New)
        }
    }

    /// A form filled from a saved connection; stored secrets are kept unless changed.
    #[must_use]
    pub fn edit(profile: &ConnectionProfile) -> Self {
        let spec = profile.spec();
        let mut form = Self::blank(FormMode::Edit(profile.id().clone()));
        form.original = Some(profile.clone());
        form.name.clone_from(&spec.name);
        form.target.clone_from(&spec.target);
        form.color = spec.color.clone().unwrap_or_default();
        form.driver = Some(spec.driver.clone());
        form.environment = spec.environment;
        form.group = profile.group().cloned();
        if let Some(tls) = &spec.tls {
            form.tls_mode = Some(tls.mode);
            form.ca_cert = path_text(tls.ca_cert.as_ref());
            form.client_cert = path_text(tls.client_cert.as_ref());
            form.client_key = path_text(tls.client_key.as_ref());
        }
        if let Some(ssh) = &spec.ssh {
            form.ssh_enabled = true;
            form.ssh_host.clone_from(&ssh.host);
            form.ssh_port = ssh.port.to_string();
            form.ssh_user.clone_from(&ssh.user);
            form.ssh_auth = SshAuthKind::of(&ssh.auth);
            if let SshAuth::KeyFile { path, .. } = &ssh.auth {
                form.ssh_key_path = path.to_string_lossy().into_owned();
            }
        }
        if profile.password_ref().is_some() {
            form.db_password = SecretField::Keep;
        }
        if profile.ssh_secret_ref().is_some() {
            form.ssh_secret = SecretField::Keep;
        }
        form
    }

    /// New or edit.
    #[must_use]
    pub fn mode(&self) -> &FormMode {
        &self.mode
    }

    /// The saved connection being edited, as it was when the form opened.
    #[must_use]
    pub fn original(&self) -> Option<&ConnectionProfile> {
        self.original.as_ref()
    }

    /// The value of a text field. The secret fields are never readable and give `""`.
    #[must_use]
    pub fn text(&self, field: FormField) -> &str {
        match field {
            FormField::Name => &self.name,
            FormField::Target => &self.target,
            FormField::Color => &self.color,
            FormField::CaCert => &self.ca_cert,
            FormField::ClientCert => &self.client_cert,
            FormField::ClientKey => &self.client_key,
            FormField::SshHost => &self.ssh_host,
            FormField::SshPort => &self.ssh_port,
            FormField::SshUser => &self.ssh_user,
            FormField::SshKeyPath => &self.ssh_key_path,
            FormField::DbPassword | FormField::SshSecret => "",
        }
    }

    /// Sets a text field. For a secret field, a non-empty text sets a new value and an empty one
    /// goes back to the stored secret (or to none when nothing is stored).
    pub fn set_text(&mut self, field: FormField, text: String) {
        match field {
            FormField::Name => self.name = text,
            FormField::Target => self.target = text,
            FormField::Color => self.color = text,
            FormField::CaCert => self.ca_cert = text,
            FormField::ClientCert => self.client_cert = text,
            FormField::ClientKey => self.client_key = text,
            FormField::SshHost => self.ssh_host = text,
            FormField::SshPort => self.ssh_port = text,
            FormField::SshUser => self.ssh_user = text,
            FormField::SshKeyPath => self.ssh_key_path = text,
            FormField::DbPassword => {
                self.db_password = Self::typed_secret(text, self.stored_db().is_some());
            }
            FormField::SshSecret => {
                self.ssh_secret = Self::typed_secret(text, self.stored_ssh().is_some());
            }
        }
        self.errors.retain(|e| e.field != Some(field));
    }

    fn typed_secret(text: String, stored: bool) -> SecretField {
        if !text.is_empty() {
            SecretField::Set(Secret::new(text))
        } else if stored {
            SecretField::Keep
        } else {
            SecretField::Clear
        }
    }

    /// Removes the stored secret on save (the "Remove" button next to a password).
    pub fn clear_secret(&mut self, slot: SecretSlot) {
        match slot {
            SecretSlot::Db => self.db_password = SecretField::Clear,
            SecretSlot::Ssh => self.ssh_secret = SecretField::Clear,
        }
    }

    /// The database password plan as it will be saved.
    #[must_use]
    pub fn db_password(&self) -> &SecretField {
        &self.db_password
    }

    /// The SSH secret plan as it will be saved.
    #[must_use]
    pub fn ssh_secret(&self) -> &SecretField {
        &self.ssh_secret
    }

    /// The stored database password reference of the saved connection, if any.
    fn stored_db(&self) -> Option<SecretRef> {
        self.original
            .as_ref()?
            .password_ref()
            .and_then(|s| from_profile_secret(&s).ok())
    }

    /// The stored SSH secret reference, only if it is of the kind the current auth needs.
    fn stored_ssh(&self) -> Option<SecretRef> {
        let needed = self.needed_ssh_kind()?;
        let stored = self.original.as_ref()?.ssh_secret_ref()?;
        let stored = from_profile_secret(&stored).ok()?;
        (stored.kind() == needed).then_some(stored)
    }

    fn needed_ssh_kind(&self) -> Option<SecretKind> {
        if self.ssh_enabled {
            self.ssh_auth.secret_kind()
        } else {
            None
        }
    }

    /// The database password plan with `Keep` checked against what is really stored.
    fn db_plan(&self) -> SecretField {
        match &self.db_password {
            SecretField::Keep if self.stored_db().is_none() => SecretField::Clear,
            other => other.clone(),
        }
    }

    /// The SSH secret plan: none without a tunnel or with the agent, `Keep` only for a stored
    /// secret of the same kind.
    fn ssh_plan(&self) -> SecretField {
        if self.needed_ssh_kind().is_none() {
            return SecretField::Clear;
        }
        match &self.ssh_secret {
            SecretField::Keep if self.stored_ssh().is_none() => SecretField::Clear,
            other => other.clone(),
        }
    }

    /// The chosen driver.
    #[must_use]
    pub fn driver(&self) -> Option<&DriverId> {
        self.driver.as_ref()
    }

    /// Chooses the driver.
    pub fn set_driver(&mut self, driver: DriverId) {
        self.driver = Some(driver);
    }

    /// The environment tag.
    #[must_use]
    pub fn environment(&self) -> Option<Environment> {
        self.environment
    }

    /// Sets the environment tag.
    pub fn set_environment(&mut self, environment: Option<Environment>) {
        self.environment = environment;
    }

    /// The group.
    #[must_use]
    pub fn group(&self) -> Option<&GroupId> {
        self.group.as_ref()
    }

    /// Chooses the group (`None` = root).
    pub fn set_group(&mut self, group: Option<GroupId>) {
        self.group = group;
    }

    /// The TLS mode; `None` means "as the target says".
    #[must_use]
    pub fn tls_mode(&self) -> Option<TlsMode> {
        self.tls_mode
    }

    /// Sets the TLS mode; `None` leaves TLS to the target (and ignores the certificate paths).
    pub fn set_tls_mode(&mut self, mode: Option<TlsMode>) {
        self.tls_mode = mode;
    }

    /// Whether an SSH tunnel is used.
    #[must_use]
    pub fn ssh_enabled(&self) -> bool {
        self.ssh_enabled
    }

    /// Turns the SSH tunnel on or off.
    pub fn set_ssh_enabled(&mut self, enabled: bool) {
        self.ssh_enabled = enabled;
    }

    /// How the tunnel authenticates.
    #[must_use]
    pub fn ssh_auth(&self) -> SshAuthKind {
        self.ssh_auth
    }

    /// Chooses the tunnel authentication.
    pub fn set_ssh_auth(&mut self, auth: SshAuthKind) {
        self.ssh_auth = auth;
    }

    /// The errors found by the last validation or save.
    #[must_use]
    pub fn errors(&self) -> &[FieldError] {
        &self.errors
    }

    /// The first error message of a field.
    #[must_use]
    pub fn error_for(&self, field: FormField) -> Option<&str> {
        self.errors
            .iter()
            .find(|e| e.field == Some(field))
            .map(|e| e.message.as_str())
    }

    /// Replaces the errors shown.
    pub fn set_errors(&mut self, errors: Vec<FieldError>) {
        self.errors = errors;
    }

    /// Checks the form and builds what to store.
    ///
    /// # Errors
    /// Every problem found, so the view can show them all at once.
    pub fn validate(&self) -> Result<ValidForm, Vec<FieldError>> {
        let mut errors = Vec::new();
        let name = self.name.trim();
        if name.is_empty() {
            errors.push(FieldError::new(FormField::Name, "name must not be empty"));
        }
        if self.driver.is_none() {
            errors.push(FieldError::general("choose a driver"));
        }
        let target = self.target.trim();
        if target.is_empty() {
            errors.push(FieldError::new(
                FormField::Target,
                "target must not be empty",
            ));
        }
        if let Some(driver) = &self.driver
            && !target.is_empty()
            && ConnectionConfig::new(name, driver.clone(), target).target_has_secret()
        {
            errors.push(FieldError::new(
                FormField::Target,
                "the target must not contain a user or password; use ?user=name and the password field",
            ));
        }
        let color = self.color.trim();
        if !color.is_empty() && Rgb::parse(color).is_none() {
            errors.push(FieldError::new(
                FormField::Color,
                "colour must look like #rrggbb",
            ));
        }
        let db_plan = self.db_plan();
        let ssh_plan = self.ssh_plan();
        let wants_password = !matches!(db_plan, SecretField::Clear);
        if (self.ssh_enabled || wants_password) && !target.is_empty() && !target.contains("://") {
            errors.push(FieldError::new(
                FormField::Target,
                "an SSH tunnel and a password need a URL target (scheme://host:port/db?user=name)",
            ));
        }
        let tls = self.validate_tls(&mut errors);
        let ssh = self.validate_ssh(&ssh_plan, &mut errors);
        if !errors.is_empty() {
            return Err(errors);
        }
        let Some(driver) = self.driver.clone() else {
            return Err(vec![FieldError::general("choose a driver")]);
        };
        let mut spec =
            ConnectionSpec::new(name, driver, target).with_password_in_keychain(wants_password);
        if !color.is_empty() {
            spec = spec.with_color(color.to_ascii_lowercase());
        }
        if let Some(env) = self.environment {
            spec = spec.with_environment(env);
        }
        if let Some(tls) = tls {
            spec = spec.with_tls(tls);
        }
        if let Some(ssh) = ssh {
            spec = spec.with_ssh(ssh);
        }
        Ok(ValidForm {
            spec,
            group: self.group.clone(),
            db_password: db_plan,
            ssh_secret: ssh_plan,
        })
    }

    fn validate_tls(&self, errors: &mut Vec<FieldError>) -> Option<TlsProfile> {
        let mode = self.tls_mode?;
        let mut tls = TlsProfile::new(mode);
        tls.ca_cert = optional_path(&self.ca_cert);
        tls.client_cert = optional_path(&self.client_cert);
        tls.client_key = optional_path(&self.client_key);
        match (&tls.client_cert, &tls.client_key) {
            (Some(_), None) => errors.push(FieldError::new(
                FormField::ClientKey,
                "a client certificate needs a client key",
            )),
            (None, Some(_)) => errors.push(FieldError::new(
                FormField::ClientCert,
                "a client key needs a client certificate",
            )),
            _ => {}
        }
        Some(tls)
    }

    fn validate_ssh(&self, plan: &SecretField, errors: &mut Vec<FieldError>) -> Option<SshProfile> {
        if !self.ssh_enabled {
            return None;
        }
        let host = self.ssh_host.trim();
        if host.is_empty() {
            errors.push(FieldError::new(
                FormField::SshHost,
                "SSH host must not be empty",
            ));
        }
        let user = self.ssh_user.trim();
        if user.is_empty() {
            errors.push(FieldError::new(
                FormField::SshUser,
                "SSH user must not be empty",
            ));
        }
        let port = match self.ssh_port.trim().parse::<u16>() {
            Ok(port) if port != 0 => port,
            _ => {
                errors.push(FieldError::new(
                    FormField::SshPort,
                    "SSH port must be a number from 1 to 65535",
                ));
                22
            }
        };
        let keeps_secret = !matches!(plan, SecretField::Clear);
        let auth = match self.ssh_auth {
            SshAuthKind::Password => {
                if !keeps_secret {
                    errors.push(FieldError::new(
                        FormField::SshSecret,
                        "an SSH password is required",
                    ));
                }
                SshAuth::Password
            }
            SshAuthKind::KeyFile => {
                let path = self.ssh_key_path.trim();
                if path.is_empty() {
                    errors.push(FieldError::new(
                        FormField::SshKeyPath,
                        "the private key file must not be empty",
                    ));
                }
                SshAuth::KeyFile {
                    path: PathBuf::from(path),
                    passphrase_in_keychain: keeps_secret,
                }
            }
            SshAuthKind::Agent => SshAuth::Agent,
        };
        let mut ssh = SshProfile::new(host, user, auth);
        ssh.port = port;
        Some(ssh)
    }

    /// The connect request for "Test connection": the form as typed, with the stored secrets
    /// referenced and the typed ones passed by value.
    ///
    /// # Errors
    /// The validation errors, or a form-wide error when a stored reference is unusable.
    pub fn connect_request(&self) -> Result<ConnectRequest, Vec<FieldError>> {
        let valid = self.validate()?;
        let source = |plan: &SecretField, stored: Option<SecretRef>| match plan {
            SecretField::Set(secret) => Ok(SecretSource::Value(secret.clone())),
            SecretField::Keep => stored.map(SecretSource::Keychain).ok_or_else(|| {
                vec![FieldError::general(
                    "the stored secret has no valid reference",
                )]
            }),
            SecretField::Clear => Ok(SecretSource::None),
        };
        Ok(ConnectRequest {
            db_password: source(&valid.db_password, self.stored_db())?,
            ssh_secret: source(&valid.ssh_secret, self.stored_ssh())?,
            spec: valid.spec,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqler_config::ConnectionStore;

    fn pg() -> DriverId {
        DriverId::new("postgres").unwrap()
    }

    fn filled() -> ConnectionForm {
        let mut f = ConnectionForm::new_connection(Some(pg()), None);
        f.set_text(FormField::Name, "Main".into());
        f.set_text(
            FormField::Target,
            "postgres://db.example:5432/app?user=me".into(),
        );
        f
    }

    fn errors_of(f: &ConnectionForm) -> Vec<(Option<FormField>, String)> {
        f.validate()
            .err()
            .unwrap_or_default()
            .into_iter()
            .map(|e| (e.field, e.message))
            .collect()
    }

    fn has_error(f: &ConnectionForm, field: FormField) -> bool {
        errors_of(f).iter().any(|(x, _)| *x == Some(field))
    }

    fn saved(spec: ConnectionSpec) -> ConnectionProfile {
        let mut store = ConnectionStore::new();
        let id = store.add_connection(spec, None).unwrap();
        store.connection(&id).unwrap().clone()
    }

    #[test]
    fn minimal_form_is_valid() {
        let v = filled().validate().unwrap();
        assert_eq!(v.spec.name, "Main");
        assert_eq!(v.spec.driver, pg());
        assert!(v.spec.tls.is_none() && v.spec.ssh.is_none());
        assert!(!v.spec.password_in_keychain);
        assert!(matches!(v.db_password, SecretField::Clear));
        assert!(v.group.is_none());
    }

    #[test]
    fn name_driver_and_target_are_required() {
        let f = ConnectionForm::new_connection(None, None);
        let errors = errors_of(&f);
        assert!(errors.iter().any(|(f, _)| *f == Some(FormField::Name)));
        assert!(errors.iter().any(|(f, _)| f.is_none()));
        assert!(errors.iter().any(|(f, _)| *f == Some(FormField::Target)));
        let mut f = filled();
        f.set_text(FormField::Name, "   ".into());
        assert!(has_error(&f, FormField::Name));
    }

    #[test]
    fn user_info_in_the_target_is_refused() {
        let mut f = filled();
        for bad in [
            "postgres://me@h/d",
            "postgres://me:pw@h/d",
            "postgres://h/d?password=x",
        ] {
            f.set_text(FormField::Target, bad.into());
            let msg = f
                .validate()
                .unwrap_err()
                .into_iter()
                .find(|e| e.field == Some(FormField::Target));
            assert!(msg.is_some(), "{bad}");
        }
        f.set_text(FormField::Target, "postgres://h/d?user=me".into());
        assert!(f.validate().is_ok());
    }

    #[test]
    fn colour_must_be_hex() {
        let mut f = filled();
        f.set_text(FormField::Color, "red".into());
        assert!(has_error(&f, FormField::Color));
        f.set_text(FormField::Color, "#AABBCC".into());
        assert_eq!(f.validate().unwrap().spec.color.as_deref(), Some("#aabbcc"));
        f.set_text(FormField::Color, String::new());
        assert_eq!(f.validate().unwrap().spec.color, None);
        for preset in COLOR_PRESETS {
            assert!(Rgb::parse(preset).is_some(), "{preset}");
        }
    }

    #[test]
    fn environment_group_and_driver_setters() {
        let mut f = filled();
        f.set_environment(Some(Environment::Prod));
        f.set_driver(DriverId::new("mysql").unwrap());
        let mut store = ConnectionStore::new();
        let g = store.add_group("g", None).unwrap();
        f.set_group(Some(g.clone()));
        let v = f.validate().unwrap();
        assert_eq!(v.spec.environment, Some(Environment::Prod));
        assert_eq!(v.spec.driver.as_str(), "mysql");
        assert_eq!(v.group, Some(g));
    }

    #[test]
    fn tls_none_means_from_target() {
        let mut f = filled();
        f.set_text(FormField::CaCert, "/ca.pem".into());
        assert!(f.validate().unwrap().spec.tls.is_none());
        f.set_tls_mode(Some(TlsMode::VerifyCa));
        let tls = f.validate().unwrap().spec.tls.unwrap();
        assert_eq!(tls.mode, TlsMode::VerifyCa);
        assert_eq!(tls.ca_cert, Some(PathBuf::from("/ca.pem")));
    }

    #[test]
    fn client_cert_and_key_come_together() {
        let mut f = filled();
        f.set_tls_mode(Some(TlsMode::Require));
        f.set_text(FormField::ClientCert, "/c.pem".into());
        assert!(has_error(&f, FormField::ClientKey));
        f.set_text(FormField::ClientCert, String::new());
        f.set_text(FormField::ClientKey, "/k.pem".into());
        assert!(has_error(&f, FormField::ClientCert));
        f.set_text(FormField::ClientCert, "/c.pem".into());
        assert!(f.validate().is_ok());
    }

    fn with_ssh(auth: SshAuthKind) -> ConnectionForm {
        let mut f = filled();
        f.set_ssh_enabled(true);
        f.set_ssh_auth(auth);
        f.set_text(FormField::SshHost, "bastion".into());
        f.set_text(FormField::SshUser, "me".into());
        f
    }

    #[test]
    fn ssh_fields_are_checked() {
        let mut f = with_ssh(SshAuthKind::Agent);
        let v = f.validate().unwrap();
        let ssh = v.spec.ssh.unwrap();
        assert_eq!(
            (ssh.host.as_str(), ssh.port, ssh.user.as_str()),
            ("bastion", 22, "me")
        );
        assert_eq!(ssh.auth, SshAuth::Agent);
        f.set_text(FormField::SshHost, " ".into());
        f.set_text(FormField::SshUser, String::new());
        assert!(has_error(&f, FormField::SshHost));
        assert!(has_error(&f, FormField::SshUser));
        for bad in ["0", "65536", "x", ""] {
            let mut f = with_ssh(SshAuthKind::Agent);
            f.set_text(FormField::SshPort, bad.into());
            assert!(has_error(&f, FormField::SshPort), "{bad:?}");
        }
        let mut f = with_ssh(SshAuthKind::Agent);
        f.set_text(FormField::SshPort, "2222".into());
        assert_eq!(f.validate().unwrap().spec.ssh.unwrap().port, 2222);
    }

    #[test]
    fn ssh_off_ignores_ssh_fields() {
        let mut f = with_ssh(SshAuthKind::Password);
        f.set_ssh_enabled(false);
        let v = f.validate().unwrap();
        assert!(v.spec.ssh.is_none());
        assert!(matches!(v.ssh_secret, SecretField::Clear));
    }

    #[test]
    fn ssh_password_is_required_and_goes_to_the_plan() {
        let mut f = with_ssh(SshAuthKind::Password);
        assert!(has_error(&f, FormField::SshSecret));
        f.set_text(FormField::SshSecret, "pw".into());
        let v = f.validate().unwrap();
        assert!(matches!(v.ssh_secret, SecretField::Set(_)));
        assert_eq!(v.spec.ssh.unwrap().auth, SshAuth::Password);
    }

    #[test]
    fn key_file_needs_a_path_and_may_have_a_passphrase() {
        let mut f = with_ssh(SshAuthKind::KeyFile);
        assert!(has_error(&f, FormField::SshKeyPath));
        f.set_text(FormField::SshKeyPath, "/home/me/.ssh/id".into());
        let v = f.validate().unwrap();
        assert_eq!(
            v.spec.ssh.unwrap().auth,
            SshAuth::KeyFile {
                path: "/home/me/.ssh/id".into(),
                passphrase_in_keychain: false
            }
        );
        f.set_text(FormField::SshSecret, "phrase".into());
        let v = f.validate().unwrap();
        assert!(matches!(
            v.spec.ssh.unwrap().auth,
            SshAuth::KeyFile {
                passphrase_in_keychain: true,
                ..
            }
        ));
    }

    #[test]
    fn password_and_tunnel_need_a_url_target() {
        let mut f = filled();
        f.set_text(FormField::Target, "app.db".into());
        assert!(f.validate().is_ok());
        f.set_text(FormField::DbPassword, "pw".into());
        assert!(has_error(&f, FormField::Target));
        f.set_text(FormField::DbPassword, String::new());
        f.set_ssh_enabled(true);
        f.set_ssh_auth(SshAuthKind::Agent);
        f.set_text(FormField::SshHost, "b".into());
        f.set_text(FormField::SshUser, "u".into());
        let msg = errors_of(&f)
            .into_iter()
            .find(|(x, _)| *x == Some(FormField::Target));
        assert_eq!(
            msg.unwrap().1,
            "an SSH tunnel and a password need a URL target (scheme://host:port/db?user=name)"
        );
        f.set_text(FormField::Target, "postgres://h/d?user=u".into());
        assert!(f.validate().is_ok());
    }

    #[test]
    fn typed_password_sets_the_keychain_flag_not_the_target() {
        let mut f = filled();
        f.set_text(FormField::DbPassword, "pw".into());
        let v = f.validate().unwrap();
        assert!(v.spec.password_in_keychain);
        assert!(matches!(v.db_password, SecretField::Set(_)));
        assert!(!v.spec.target.contains("pw"));
        assert!(!format!("{f:?}").contains("\"pw\""));
    }

    fn saved_with_password() -> ConnectionProfile {
        saved(
            ConnectionSpec::new("Main", pg(), "postgres://db/app?user=me")
                .with_password_in_keychain(true),
        )
    }

    #[test]
    fn edit_fills_the_form_and_keeps_the_stored_password() {
        let p = saved_with_password();
        let f = ConnectionForm::edit(&p);
        assert_eq!(f.mode(), &FormMode::Edit(p.id().clone()));
        assert_eq!(f.text(FormField::Name), "Main");
        assert_eq!(f.text(FormField::DbPassword), "");
        assert!(matches!(f.db_password(), SecretField::Keep));
        let v = f.validate().unwrap();
        assert!(v.spec.password_in_keychain);
        assert!(matches!(v.db_password, SecretField::Keep));
    }

    #[test]
    fn secret_matrix_for_the_database_password() {
        // Edit with a stored password: Keep / Set / Clear.
        let mut f = ConnectionForm::edit(&saved_with_password());
        f.set_text(FormField::DbPassword, "new".into());
        assert!(matches!(
            f.validate().unwrap().db_password,
            SecretField::Set(_)
        ));
        f.set_text(FormField::DbPassword, String::new());
        assert!(matches!(
            f.validate().unwrap().db_password,
            SecretField::Keep
        ));
        f.clear_secret(SecretSlot::Db);
        let v = f.validate().unwrap();
        assert!(matches!(v.db_password, SecretField::Clear));
        assert!(!v.spec.password_in_keychain);
        // Edit without a stored password: empty stays Clear, typing is Set.
        let p = saved(ConnectionSpec::new("x", pg(), "postgres://h/d?user=u"));
        let mut f = ConnectionForm::edit(&p);
        assert!(matches!(
            f.validate().unwrap().db_password,
            SecretField::Clear
        ));
        f.set_text(FormField::DbPassword, "pw".into());
        assert!(matches!(
            f.validate().unwrap().db_password,
            SecretField::Set(_)
        ));
        f.set_text(FormField::DbPassword, String::new());
        assert!(matches!(
            f.validate().unwrap().db_password,
            SecretField::Clear
        ));
        // New: empty is Clear.
        assert!(matches!(
            filled().validate().unwrap().db_password,
            SecretField::Clear
        ));
    }

    #[test]
    fn keep_is_not_valid_without_a_stored_reference() {
        let mut f = filled();
        f.db_password = SecretField::Keep;
        assert!(matches!(
            f.validate().unwrap().db_password,
            SecretField::Clear
        ));
    }

    fn saved_with_ssh(auth: SshAuth) -> ConnectionProfile {
        saved(
            ConnectionSpec::new("Tun", pg(), "postgres://db/app?user=me")
                .with_ssh(SshProfile::new("bastion", "me", auth)),
        )
    }

    #[test]
    fn ssh_secret_kept_only_for_the_same_auth_kind() {
        let p = saved_with_ssh(SshAuth::Password);
        let mut f = ConnectionForm::edit(&p);
        assert!(matches!(f.ssh_secret(), SecretField::Keep));
        assert!(matches!(
            f.validate().unwrap().ssh_secret,
            SecretField::Keep
        ));
        // Switching to a key file drops the stored password.
        f.set_ssh_auth(SshAuthKind::KeyFile);
        f.set_text(FormField::SshKeyPath, "/k".into());
        let v = f.validate().unwrap();
        assert!(matches!(v.ssh_secret, SecretField::Clear));
        // Switching to the agent clears it too.
        f.set_ssh_auth(SshAuthKind::Agent);
        assert!(matches!(
            f.validate().unwrap().ssh_secret,
            SecretField::Clear
        ));
        // Back to password: kept again, a typed one replaces it.
        f.set_ssh_auth(SshAuthKind::Password);
        assert!(matches!(
            f.validate().unwrap().ssh_secret,
            SecretField::Keep
        ));
        f.clear_secret(SecretSlot::Ssh);
        assert!(has_error(&f, FormField::SshSecret));
    }

    #[test]
    fn edit_with_key_passphrase_round_trips() {
        let p = saved_with_ssh(SshAuth::KeyFile {
            path: "/k".into(),
            passphrase_in_keychain: true,
        });
        let f = ConnectionForm::edit(&p);
        assert_eq!(f.ssh_auth(), SshAuthKind::KeyFile);
        assert_eq!(f.text(FormField::SshKeyPath), "/k");
        let v = f.validate().unwrap();
        assert!(matches!(v.ssh_secret, SecretField::Keep));
        assert_eq!(v.spec.ssh.unwrap(), p.spec().ssh.clone().unwrap());
    }

    #[test]
    fn connect_request_maps_the_plans() {
        // Typed values travel by value.
        let mut f = filled();
        f.set_text(FormField::DbPassword, "pw".into());
        let r = f.connect_request().unwrap();
        assert!(matches!(r.db_password, SecretSource::Value(_)));
        assert!(matches!(r.ssh_secret, SecretSource::None));
        // Kept ones are referenced.
        let p = saved_with_password();
        let f = ConnectionForm::edit(&p);
        let r = f.connect_request().unwrap();
        let SecretSource::Keychain(rf) = r.db_password else {
            panic!("expected keychain");
        };
        assert_eq!(rf.to_string(), "main:db-password");
        // Cleared ones are not used.
        let mut f = ConnectionForm::edit(&p);
        f.clear_secret(SecretSlot::Db);
        assert!(matches!(
            f.connect_request().unwrap().db_password,
            SecretSource::None
        ));
        // An invalid form gives its errors.
        let f = ConnectionForm::new_connection(None, None);
        assert!(f.connect_request().is_err());
    }

    #[test]
    fn ssh_connect_request_uses_the_right_secret() {
        let mut f = with_ssh(SshAuthKind::Password);
        f.set_text(FormField::SshSecret, "sshpw".into());
        let r = f.connect_request().unwrap();
        assert!(matches!(r.ssh_secret, SecretSource::Value(_)));
        assert!(r.spec.ssh.is_some());
    }

    #[test]
    fn set_text_clears_the_field_error() {
        let mut f = ConnectionForm::new_connection(None, None);
        f.set_errors(f.validate().unwrap_err());
        assert!(f.error_for(FormField::Name).is_some());
        f.set_text(FormField::Name, "x".into());
        assert!(f.error_for(FormField::Name).is_none());
        assert!(f.error_for(FormField::Target).is_some());
    }

    #[test]
    fn store_errors_map_to_fields() {
        let name = FieldError::from_store(&StoreError::DuplicateConnectionName("a".into()));
        assert_eq!(name.field, Some(FormField::Name));
        let target = FieldError::from_store(&StoreError::TargetHasSecret);
        assert_eq!(target.field, Some(FormField::Target));
        let color = FieldError::from_store(&StoreError::InvalidColor("x".into()));
        assert_eq!(color.field, Some(FormField::Color));
        let other = FieldError::from_store(&StoreError::GroupNotEmpty(
            ConnectionStore::new().add_group("a", None).unwrap(),
        ));
        assert_eq!(other.field, None);
    }

    #[test]
    fn test_status_starts_idle() {
        assert_eq!(filled().test, TestStatus::Idle);
    }
}
