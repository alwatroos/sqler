// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Conversions between the generated WIT types and the host's own types.

use sqler_plugin_api::SettingValue;

use super::bindings::sqler::plugin::{settings as wit_settings, types as wit};
use crate::error::{ErrorCode, GuestError};

/// Exhaustive on purpose: a case added to the WIT `error-code` stops this from compiling until `ErrorCode` follows.
impl From<wit::ErrorCode> for ErrorCode {
    fn from(code: wit::ErrorCode) -> Self {
        match code {
            wit::ErrorCode::CapabilityDenied => Self::CapabilityDenied,
            wit::ErrorCode::Declined => Self::Declined,
            wit::ErrorCode::InvalidArgument => Self::InvalidArgument,
            wit::ErrorCode::NotFound => Self::NotFound,
            wit::ErrorCode::LimitExceeded => Self::LimitExceeded,
            wit::ErrorCode::Timeout => Self::Timeout,
            wit::ErrorCode::Failed => Self::Failed,
        }
    }
}

impl From<ErrorCode> for wit::ErrorCode {
    fn from(code: ErrorCode) -> Self {
        match code {
            ErrorCode::CapabilityDenied => Self::CapabilityDenied,
            ErrorCode::Declined => Self::Declined,
            ErrorCode::InvalidArgument => Self::InvalidArgument,
            ErrorCode::NotFound => Self::NotFound,
            ErrorCode::LimitExceeded => Self::LimitExceeded,
            ErrorCode::Timeout => Self::Timeout,
            ErrorCode::Failed => Self::Failed,
        }
    }
}

impl From<wit::PluginError> for GuestError {
    fn from(e: wit::PluginError) -> Self {
        Self {
            code: e.code.into(),
            message: e.message,
        }
    }
}

/// `types` only declares types; the trait is empty but the linker wants it.
impl wit::Host for super::state::HostState {}

/// A WIT error for a refused or failed host call.
pub(crate) fn plugin_error(code: ErrorCode, message: impl Into<String>) -> wit::PluginError {
    wit::PluginError {
        code: code.into(),
        message: message.into(),
    }
}

/// The WIT view of a setting value. `None` for a variant this host cannot express (the manifest type is
/// non-exhaustive).
pub(crate) fn setting_to_wit(v: SettingValue) -> Option<wit_settings::SettingValue> {
    Some(match v {
        SettingValue::Boolean(b) => wit_settings::SettingValue::Boolean(b),
        SettingValue::Integer(i) => wit_settings::SettingValue::Integer(i),
        SettingValue::Float(f) => wit_settings::SettingValue::Float(f),
        SettingValue::Text(t) => wit_settings::SettingValue::Text(t),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [ErrorCode; 7] = [
        ErrorCode::CapabilityDenied,
        ErrorCode::Declined,
        ErrorCode::InvalidArgument,
        ErrorCode::NotFound,
        ErrorCode::LimitExceeded,
        ErrorCode::Timeout,
        ErrorCode::Failed,
    ];

    #[test]
    fn error_code_round_trips_for_every_case() {
        for code in ALL {
            let wit: wit::ErrorCode = code.into();
            assert_eq!(ErrorCode::from(wit), code);
        }
    }

    #[test]
    fn plugin_error_converts_to_a_guest_error() {
        let g: GuestError = plugin_error(ErrorCode::Declined, "no").into();
        assert_eq!(g.code, ErrorCode::Declined);
        assert_eq!(g.message, "no");
    }

    #[test]
    fn settings_convert() {
        assert!(matches!(
            setting_to_wit(SettingValue::Text("a".into())),
            Some(wit_settings::SettingValue::Text(t)) if t == "a"
        ));
    }
}
