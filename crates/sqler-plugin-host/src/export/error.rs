// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The fixed texts of plugin export formats and the mapping of call failures to [`ExportError`].
//!
//! A text names the format and the plugin by label and id, and never carries engine output, a path or SQL. Only a
//! guest's own message is passed on, cleaned to one line and capped.

use sqler_export::ExportError;

use crate::error::{CallError, GuestError};
use crate::ui::limits::one_line;

/// Bytes of a guest message that reach the user.
const GUEST_MESSAGE_BYTES: usize = 256;

/// The guest trapped.
pub(crate) fn crashed(format: &str, plugin: &str) -> String {
    format!("the export format `{format}` of plugin `{plugin}` crashed")
}

/// The guest missed its deadline.
pub(crate) fn timed_out(format: &str, plugin: &str) -> String {
    format!("the export format `{format}` of plugin `{plugin}` did not answer in time")
}

/// The guest hit its memory cap.
pub(crate) fn out_of_memory(format: &str, plugin: &str) -> String {
    format!("the export format `{format}` of plugin `{plugin}` ran out of memory")
}

/// The instance the export lives in is gone, or the plugin was disabled.
pub(crate) fn stopped(format: &str, plugin: &str) -> String {
    format!(
        "the export format `{format}` of plugin `{plugin}` stopped (the plugin was restarted, disabled or its permissions changed)"
    )
}

/// The plugin cannot serve the call at all (no such export, not loadable, engine failure).
pub(crate) fn not_available(format: &str, plugin: &str) -> String {
    format!("the export format `{format}` of plugin `{plugin}` is not available")
}

/// The guest returned a `plugin-error`.
pub(crate) fn failed(format: &str, plugin: &str, e: &GuestError) -> String {
    format!(
        "the export format `{format}` of plugin `{plugin}` failed: {}",
        one_line(&e.message, GUEST_MESSAGE_BYTES)
    )
}

/// A guest error as an [`ExportError`].
pub(crate) fn guest(e: &GuestError, format: &str, plugin: &str) -> ExportError {
    ExportError::Plugin(failed(format, plugin, e))
}

/// A failed call as an [`ExportError`]. Engine text never passes through.
pub(crate) fn from_call(e: &CallError, format: &str, plugin: &str) -> ExportError {
    let text = match e {
        CallError::Cancelled => return ExportError::Cancelled,
        CallError::Trap { .. } => crashed(format, plugin),
        CallError::Timeout { .. } => timed_out(format, plugin),
        CallError::MemoryLimit { .. } => out_of_memory(format, plugin),
        CallError::Disabled | CallError::NotFound => stopped(format, plugin),
        CallError::Guest(g) => failed(format, plugin, g),
        _ => not_available(format, plugin),
    };
    ExportError::Plugin(text)
}

/// Whether a failed call leaves the plugin without the instance the export lives in.
pub(crate) fn kills_instance(e: &CallError) -> bool {
    matches!(
        e,
        CallError::Trap { .. }
            | CallError::MemoryLimit { .. }
            | CallError::Timeout { .. }
            | CallError::Cancelled
            | CallError::HostPanic
            | CallError::Engine { .. }
            | CallError::Disabled
            | CallError::NotFound
    )
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::error::{ErrorCode, LoadError};

    fn text(e: &ExportError) -> String {
        match e {
            ExportError::Plugin(t) => t.clone(),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn call_failures_map_to_fixed_texts() {
        let f = "Fixture (plugin): TSV";
        let p = "fx";
        let cases: Vec<(CallError, String)> = vec![
            (
                CallError::Trap {
                    message: "wasm trap at /home/x/engine.rs:1".into(),
                },
                "the export format `Fixture (plugin): TSV` of plugin `fx` crashed".into(),
            ),
            (
                CallError::Timeout {
                    after: Duration::from_secs(5),
                },
                "the export format `Fixture (plugin): TSV` of plugin `fx` did not answer in time"
                    .into(),
            ),
            (
                CallError::MemoryLimit { limit: 1 },
                "the export format `Fixture (plugin): TSV` of plugin `fx` ran out of memory".into(),
            ),
            (
                CallError::Disabled,
                "the export format `Fixture (plugin): TSV` of plugin `fx` stopped (the plugin was restarted, disabled or its permissions changed)".into(),
            ),
            (
                CallError::MissingExport {
                    interface: "exporter",
                },
                "the export format `Fixture (plugin): TSV` of plugin `fx` is not available".into(),
            ),
            (
                CallError::Engine {
                    message: "secret engine detail".into(),
                },
                "the export format `Fixture (plugin): TSV` of plugin `fx` is not available".into(),
            ),
            (
                CallError::Load(LoadError::Compile {
                    message: "/tmp/x".into(),
                }),
                "the export format `Fixture (plugin): TSV` of plugin `fx` is not available".into(),
            ),
        ];
        for (e, want) in cases {
            assert_eq!(text(&from_call(&e, f, p)), want, "{e:?}");
        }
    }

    #[test]
    fn cancelled_stays_cancelled() {
        assert!(matches!(
            from_call(&CallError::Cancelled, "f", "p"),
            ExportError::Cancelled
        ));
    }

    #[test]
    fn a_guest_message_is_one_line_clean_and_capped() {
        let message = format!("bad\nnews\u{202E}here {}", "x".repeat(400));
        let e = GuestError {
            code: ErrorCode::Failed,
            message,
        };
        let t = text(&guest(&e, "F", "p"));
        let prefix = "the export format `F` of plugin `p` failed: ";
        assert!(t.starts_with(prefix));
        let tail = &t[prefix.len()..];
        assert!(tail.starts_with("bad newshere "), "{tail}");
        assert!(!t.contains(['\n', '\u{202E}']));
        assert!(tail.len() <= GUEST_MESSAGE_BYTES);
    }

    #[test]
    fn instance_killers() {
        assert!(kills_instance(&CallError::Cancelled));
        assert!(kills_instance(&CallError::Trap {
            message: String::new()
        }));
        assert!(!kills_instance(&CallError::MissingExport {
            interface: "exporter"
        }));
    }
}
