// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `DropFlow`: the confirmation steps before a `DROP` runs.
//!
//! The SQL comes from `drop_script` once, is shown in the dialog, and is the only text
//! [`DropFlow::confirm`] ever hands out. A production connection needs a second confirmation,
//! which is ignored during the first [`PROD_GUARD`] after the step appeared, so a double click
//! or a key repeat that passed the first step cannot pass the second.

use std::time::{Duration, Instant};

use sqler_config::ConnectionId;
use sqler_driver_api::sqler_core::DbObject;
use sqler_sql::{DdlError, Dialect, drop_script, qualified_name};

/// How long the production step ignores a confirmation after it appeared.
pub const PROD_GUARD: Duration = Duration::from_millis(800);

/// Where the confirmation is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DropStep {
    /// The first confirmation.
    Confirm,
    /// The second confirmation, for production connections.
    ConfirmProd,
    /// The statement was handed out and runs now.
    Running,
}

/// What the DROP dialog shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DropDialog {
    /// The heading.
    pub title: String,
    /// The explanation.
    pub body: String,
    /// The exact statement that will run.
    pub sql: String,
    /// The label of the confirm button; `None` while running (no buttons).
    pub confirm_label: Option<&'static str>,
}

/// One DROP in progress: the object, the statement and the confirmation step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DropFlow {
    connection: ConnectionId,
    connection_name: String,
    object: DbObject,
    name: String,
    sql: String,
    prod: bool,
    step: DropStep,
    /// When the production step appeared.
    prod_since: Option<Instant>,
}

impl DropFlow {
    /// Starts the flow at the first confirmation.
    ///
    /// # Errors
    /// [`DdlError`] when the dialect cannot drop this object or the name is invalid.
    pub fn start(
        connection: ConnectionId,
        connection_name: String,
        object: DbObject,
        dialect: Dialect,
        prod: bool,
    ) -> Result<Self, DdlError> {
        let sql = drop_script(dialect, &object)?;
        let name = qualified_name(dialect, &object)?;
        Ok(Self {
            connection,
            connection_name,
            object,
            name,
            sql,
            prod,
            step: DropStep::Confirm,
            prod_since: None,
        })
    }

    /// The current step.
    #[must_use]
    pub fn step(&self) -> DropStep {
        self.step
    }

    /// The statement the dialog shows and `confirm` returns.
    #[must_use]
    pub fn sql(&self) -> &str {
        &self.sql
    }

    /// The object to drop.
    #[must_use]
    pub fn object(&self) -> &DbObject {
        &self.object
    }

    /// The connection the object lives on.
    #[must_use]
    pub fn connection(&self) -> &ConnectionId {
        &self.connection
    }

    /// Whether the dialog can be closed: not once the statement was handed out.
    #[must_use]
    pub fn can_cancel(&self) -> bool {
        self.step != DropStep::Running
    }

    /// What to show for the current step.
    #[must_use]
    pub fn dialog(&self) -> DropDialog {
        let kind = self.object.kind.as_str();
        let (title, body, confirm_label) = match self.step {
            DropStep::Confirm => (
                format!("Drop {kind} {}?", self.name),
                "This runs the statement below. It cannot be undone.".to_owned(),
                Some("Drop"),
            ),
            DropStep::ConfirmProd => (
                "Production connection".to_owned(),
                format!(
                    "\"{}\" is a production connection. Drop {kind} {} there?",
                    self.connection_name, self.name
                ),
                Some("Drop on production"),
            ),
            DropStep::Running => (
                format!("Drop {kind} {}?", self.name),
                "Dropping\u{2026}".to_owned(),
                None,
            ),
        };
        DropDialog {
            title,
            body,
            sql: self.sql.clone(),
            confirm_label,
        }
    }

    /// Confirms the current step. Returns the statement to run, once, when the last
    /// confirmation is given; `None` while a further confirmation is needed, when running, or
    /// when the production step is confirmed within [`PROD_GUARD`] of its appearing.
    pub fn confirm(&mut self) -> Option<String> {
        self.confirm_at(Instant::now())
    }

    /// Pretends the production step appeared [`PROD_GUARD`] ago (for tests of the workspace).
    #[cfg(test)]
    pub(crate) fn age_prod_step(&mut self) {
        self.prod_since = self.prod_since.and_then(|t| t.checked_sub(PROD_GUARD));
    }

    /// [`DropFlow::confirm`] with the time given (for tests).
    pub fn confirm_at(&mut self, now: Instant) -> Option<String> {
        match self.step {
            DropStep::Confirm if self.prod => {
                self.step = DropStep::ConfirmProd;
                self.prod_since = Some(now);
                None
            }
            DropStep::ConfirmProd
                if self
                    .prod_since
                    .is_some_and(|t| now.saturating_duration_since(t) < PROD_GUARD) =>
            {
                None
            }
            DropStep::Confirm | DropStep::ConfirmProd => {
                self.step = DropStep::Running;
                Some(self.sql.clone())
            }
            DropStep::Running => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use sqler_config::{ConnectionSpec, ConnectionStore};
    use sqler_driver_api::sqler_core::{DriverId, ObjectKind, ObjectPath};

    use super::*;

    fn conn() -> ConnectionId {
        let mut store = ConnectionStore::new();
        let spec = ConnectionSpec::new("c1", DriverId::new("sqlite").unwrap(), ":memory:");
        store.add_connection(spec, None).unwrap()
    }

    fn table(name: &str) -> DbObject {
        DbObject::new(
            ObjectKind::Table,
            ObjectPath::root().in_schema("public"),
            name,
        )
    }

    fn flow(prod: bool) -> DropFlow {
        DropFlow::start(
            conn(),
            "Main".into(),
            table("users"),
            Dialect::Postgres,
            prod,
        )
        .unwrap()
    }

    #[test]
    fn non_prod_needs_one_confirm() {
        let mut f = flow(false);
        assert_eq!(f.step(), DropStep::Confirm);
        assert!(f.can_cancel());
        let sql = f.confirm().unwrap();
        assert_eq!(sql, "DROP TABLE \"public\".\"users\";\n");
        assert_eq!(f.step(), DropStep::Running);
        assert!(!f.can_cancel());
    }

    #[test]
    fn prod_needs_two_confirms() {
        let mut f = flow(true);
        assert_eq!(f.confirm(), None);
        assert_eq!(f.step(), DropStep::ConfirmProd);
        assert!(f.can_cancel());
        let sql = f.confirm_at(Instant::now() + PROD_GUARD).unwrap();
        assert_eq!(sql, f.sql());
        assert_eq!(f.step(), DropStep::Running);
    }

    #[test]
    fn the_prod_step_ignores_a_confirm_right_after_it_appeared() {
        let t0 = Instant::now();
        let mut f = flow(true);
        assert_eq!(f.confirm_at(t0), None);
        // The second click of a double click, or a key repeat.
        assert_eq!(f.confirm_at(t0), None);
        assert_eq!(f.confirm_at(t0 + PROD_GUARD / 2), None);
        assert_eq!(f.step(), DropStep::ConfirmProd);
        assert!(f.can_cancel());
        // A deliberate later confirm passes.
        let sql = f.confirm_at(t0 + PROD_GUARD).unwrap();
        assert_eq!(sql, f.sql());
        assert_eq!(f.step(), DropStep::Running);
    }

    #[test]
    fn a_dev_connection_is_not_delayed() {
        let mut f = flow(false);
        assert!(f.confirm_at(Instant::now()).is_some());
    }

    #[test]
    fn the_returned_sql_is_the_shown_sql() {
        let mut f = flow(true);
        let shown_first = f.dialog().sql;
        let t0 = Instant::now();
        f.confirm_at(t0);
        let shown_second = f.dialog().sql;
        let run = f.confirm_at(t0 + PROD_GUARD).unwrap();
        assert_eq!(shown_first, run);
        assert_eq!(shown_second, run);
        assert_eq!(f.dialog().sql, run);
    }

    #[test]
    fn confirm_while_running_gives_nothing() {
        let mut f = flow(false);
        f.confirm().unwrap();
        assert_eq!(f.confirm(), None);
        assert_eq!(f.confirm(), None);
        assert_eq!(f.step(), DropStep::Running);
    }

    #[test]
    fn dialog_texts() {
        let mut f = flow(true);
        let d = f.dialog();
        assert_eq!(d.title, "Drop table \"public\".\"users\"?");
        assert_eq!(
            d.body,
            "This runs the statement below. It cannot be undone."
        );
        assert_eq!(d.confirm_label, Some("Drop"));
        let t0 = Instant::now();
        f.confirm_at(t0);
        let d = f.dialog();
        assert_eq!(d.title, "Production connection");
        assert_eq!(
            d.body,
            "\"Main\" is a production connection. Drop table \"public\".\"users\" there?"
        );
        assert_eq!(d.confirm_label, Some("Drop on production"));
        f.confirm_at(t0 + PROD_GUARD);
        let d = f.dialog();
        assert_eq!(d.body, "Dropping\u{2026}");
        assert_eq!(d.confirm_label, None);
    }

    #[test]
    fn names_with_quotes_are_quoted() {
        let f = DropFlow::start(
            conn(),
            "c".into(),
            table("we\"ird"),
            Dialect::Postgres,
            false,
        )
        .unwrap();
        assert_eq!(f.sql(), "DROP TABLE \"public\".\"we\"\"ird\";\n");
        let f = DropFlow::start(conn(), "c".into(), table("a`b"), Dialect::MySql, false).unwrap();
        assert!(f.sql().contains("`a``b`"), "{}", f.sql());
    }

    #[test]
    fn start_fails_for_a_bad_name_or_unsupported_object() {
        assert!(DropFlow::start(conn(), "c".into(), table(""), Dialect::Postgres, false).is_err());
        let ext = DbObject::new(ObjectKind::Extension, ObjectPath::root(), "x");
        assert!(DropFlow::start(conn(), "c".into(), ext, Dialect::Sqlite, false).is_err());
    }

    #[test]
    fn accessors() {
        let f = flow(false);
        assert_eq!(f.object().name, "users");
        assert_eq!(f.connection(), &conn());
    }
}
