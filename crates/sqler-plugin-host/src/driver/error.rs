// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The fixed texts of plugin drivers (ids only, never engine or guest text) and the mapping of call failures to
//! [`DriverError`].

use std::fmt::Write as _;

use sqler_driver_api::DriverError;

use super::params::ConnectParams;
use crate::error::{CallError, GuestError};
use crate::query::{DENIED, query_text};

/// A session of a guest driver that outlived its instance.
pub(crate) const SESSION_ENDED: &str = "the plugin driver session ended (the plugin was restarted, disabled or its permissions changed)";
/// The guest trapped.
pub(crate) const CRASHED: &str = "the plugin driver crashed";
/// The guest missed a deadline.
pub(crate) const TIMED_OUT: &str = "the plugin driver did not answer in time";
/// The guest hit its memory cap.
pub(crate) const OUT_OF_MEMORY: &str = "the plugin driver ran out of memory";
/// The plugin cannot serve a call at all (not loadable, no such export, engine failure).
pub(crate) const NOT_AVAILABLE: &str = "the plugin driver is not available";
/// A batch that breaks the stream rules.
pub(crate) const MALFORMED: &str = "the plugin driver sent malformed rows";
/// A catalog answer that breaks the size rules.
pub(crate) const MALFORMED_CATALOG: &str = "the plugin driver sent a malformed catalog answer";

/// The ids of the built-in drivers. A plugin never provides them, also in a build without that built-in.
pub(crate) const RESERVED_IDS: &[&str] = &["sqlite", "postgres", "mysql"];

/// Rule 0: the id belongs to a built-in driver.
pub(crate) fn reserved(driver: &str, plugin: &str) -> String {
    format!("driver `{driver}` of plugin `{plugin}` is reserved for a built-in driver")
}

/// Rule 1: the plugin is switched off or was disabled after faults.
pub(crate) fn disabled(driver: &str, plugin: &str) -> String {
    format!("driver `{driver}` comes from plugin `{plugin}`, which is disabled")
}

/// Rule 2: the permission is not effective.
pub(crate) fn not_approved(driver: &str, plugin: &str, capability: &str) -> String {
    format!(
        "driver `{driver}` needs the permission `{capability}` of plugin `{plugin}`, which is not approved"
    )
}

/// Rule 3: the build has no such host engine.
pub(crate) fn no_engine(driver: &str, engine: &str) -> String {
    format!("driver `{driver}` needs the `{engine}` engine, which this build does not include")
}

/// Rule 4: an earlier plugin owns the id.
pub(crate) fn collision(driver: &str, other: &str) -> String {
    format!("driver `{driver}` is already provided by plugin `{other}`")
}

/// Which call a failure belongs to: the text goes to `Connection` while connecting and `Query` after.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    Connect,
    Run,
}

/// A fixed text as the error of `phase`.
pub(crate) fn fixed(text: &str, phase: Phase) -> DriverError {
    match phase {
        Phase::Connect => DriverError::Connection(text.to_owned()),
        Phase::Run => DriverError::Query(text.to_owned()),
    }
}

/// The forms of the connect password (raw, percent-encoded in upper and lower case) a guest message must not carry.
/// Empty for a missing or empty password.
pub(crate) fn secrets(params: &ConnectParams) -> Vec<String> {
    let Some(password) = params.password.as_deref().filter(|p| !p.is_empty()) else {
        return Vec::new();
    };
    let encode = |upper: bool| {
        let mut out = String::new();
        for b in password.bytes() {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
                out.push(char::from(b));
            } else if upper {
                let _ = write!(out, "%{b:02X}");
            } else {
                let _ = write!(out, "%{b:02x}");
            }
        }
        out
    };
    let mut all = vec![password.to_owned(), encode(true), encode(false)];
    all.dedup();
    all
}

/// An error the guest returned. The text passes the query scrub, so a message that echoes a secret becomes the fixed
/// denial text.
pub(crate) fn guest(e: &GuestError, phase: Phase, secrets: &[String]) -> DriverError {
    let text = if secrets.iter().any(|s| e.message.contains(s.as_str())) {
        DENIED.to_owned()
    } else {
        query_text(&e.message)
    };
    match phase {
        Phase::Connect => DriverError::Connection(text),
        Phase::Run => DriverError::Query(text),
    }
}

/// A failed call. Engine text never passes through.
pub(crate) fn from_call(e: &CallError, phase: Phase, secrets: &[String]) -> DriverError {
    match e {
        CallError::Trap { .. } => fixed(CRASHED, phase),
        CallError::Timeout { .. } => fixed(TIMED_OUT, phase),
        CallError::MemoryLimit { .. } => fixed(OUT_OF_MEMORY, phase),
        CallError::Cancelled => DriverError::Cancelled,
        CallError::Guest(g) => guest(g, phase, secrets),
        _ => DriverError::Unavailable(NOT_AVAILABLE.to_owned()),
    }
}

/// Whether a failed call leaves the plugin without the instance its sessions live in.
pub(crate) fn kills_instance(e: &CallError) -> bool {
    matches!(
        e,
        CallError::Trap { .. }
            | CallError::MemoryLimit { .. }
            | CallError::Timeout { .. }
            | CallError::Cancelled
            | CallError::HostPanic
            | CallError::Engine { .. }
    )
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::error::{ErrorCode, LoadError};

    fn text(e: &DriverError) -> String {
        match e {
            DriverError::Connection(t) | DriverError::Query(t) | DriverError::Unavailable(t) => {
                t.clone()
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn availability_texts_are_fixed() {
        assert_eq!(
            disabled("d", "p"),
            "driver `d` comes from plugin `p`, which is disabled"
        );
        assert_eq!(
            not_approved("d", "p", "driver.guest"),
            "driver `d` needs the permission `driver.guest` of plugin `p`, which is not approved"
        );
        assert_eq!(
            no_engine("d", "duckdb"),
            "driver `d` needs the `duckdb` engine, which this build does not include"
        );
        assert_eq!(
            collision("d", "q"),
            "driver `d` is already provided by plugin `q`"
        );
    }

    #[test]
    fn call_failures_map_to_fixed_texts() {
        let trap = CallError::Trap {
            message: "wasm trap at 0x1234 secret".into(),
        };
        assert_eq!(text(&from_call(&trap, Phase::Run, &[])), CRASHED);
        assert!(matches!(
            from_call(&trap, Phase::Connect, &[]),
            DriverError::Connection(_)
        ));
        let timeout = CallError::Timeout {
            after: Duration::from_secs(1),
        };
        assert_eq!(text(&from_call(&timeout, Phase::Run, &[])), TIMED_OUT);
        let memory = CallError::MemoryLimit { limit: 1 };
        assert_eq!(text(&from_call(&memory, Phase::Run, &[])), OUT_OF_MEMORY);
        assert!(matches!(
            from_call(&CallError::Cancelled, Phase::Run, &[]),
            DriverError::Cancelled
        ));
        for e in [
            CallError::Disabled,
            CallError::NotFound,
            CallError::MissingExport {
                interface: "driver",
            },
            CallError::HostPanic,
            CallError::Engine {
                message: "boom at /home/x".into(),
            },
            CallError::Load(LoadError::NoCompiler),
        ] {
            let mapped = from_call(&e, Phase::Run, &[]);
            assert!(matches!(&mapped, DriverError::Unavailable(t) if t == NOT_AVAILABLE));
        }
    }

    #[test]
    fn a_guest_error_is_scrubbed() {
        let g = GuestError {
            code: ErrorCode::Failed,
            message: "login failed, password was hunter2".into(),
        };
        let mapped = guest(&g, Phase::Connect, &[]);
        assert!(matches!(&mapped, DriverError::Connection(t) if !t.contains("hunter2")));
        let plain = GuestError {
            code: ErrorCode::Failed,
            message: "no such table t".into(),
        };
        assert!(
            matches!(guest(&plain, Phase::Run, &[]), DriverError::Query(t) if t == "no such table t")
        );
    }

    #[test]
    fn an_echoed_password_value_is_denied_without_a_marker_word() {
        let params = ConnectParams::parse("mydb://u:p%40ss%2Fw@h/db");
        let secrets = secrets(&params);
        assert!(secrets.contains(&"p@ss/w".to_owned()));
        for message in [
            "connect failed: p@ss/w",
            "bad token p%40ss%2Fw for db",
            "x p%40ss%2fw",
        ] {
            let g = GuestError {
                code: ErrorCode::Failed,
                message: message.into(),
            };
            for phase in [Phase::Connect, Phase::Run] {
                let t = text(&guest(&g, phase, &secrets));
                assert_eq!(t, DENIED, "{message}");
            }
        }
        let g = GuestError {
            code: ErrorCode::Failed,
            message: "no such table t".into(),
        };
        assert_eq!(text(&guest(&g, Phase::Run, &secrets)), "no such table t");
        let none: Vec<String> = Vec::new();
        assert_eq!(
            super::secrets(&ConnectParams::parse("mydb://u:@h/db")),
            none
        );
    }

    #[test]
    fn only_a_guest_error_keeps_the_instance() {
        assert!(!kills_instance(&CallError::Guest(GuestError {
            code: ErrorCode::Failed,
            message: String::new(),
        })));
        assert!(kills_instance(&CallError::Cancelled));
        assert!(kills_instance(&CallError::HostPanic));
        assert!(!kills_instance(&CallError::MissingExport {
            interface: "driver"
        }));
    }
}
