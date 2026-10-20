// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Theme resolution: turns the saved [`ThemePreference`] and the OS appearance into the light or
//! dark look. Plain Rust; the `GPUI` mapping lives in `apply` (feature `gui`).

#[cfg(feature = "gui")]
pub(crate) mod apply;

use sqler_config::ThemePreference;

/// The look the shell is drawn in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Appearance {
    /// Light colours.
    #[default]
    Light,
    /// Dark colours.
    Dark,
}

/// The look for a preference: `Light` and `Dark` are explicit, `System` (and any future
/// variant) follows `system`.
#[must_use]
pub fn resolve(pref: ThemePreference, system: Appearance) -> Appearance {
    match pref {
        ThemePreference::Light => Appearance::Light,
        ThemePreference::Dark => Appearance::Dark,
        _ => system,
    }
}

/// The explicit preference that shows the opposite of `resolve(pref, system)`.
#[must_use]
pub fn toggled(pref: ThemePreference, system: Appearance) -> ThemePreference {
    match resolve(pref, system) {
        Appearance::Light => ThemePreference::Dark,
        Appearance::Dark => ThemePreference::Light,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PREFS: [ThemePreference; 3] = [
        ThemePreference::Light,
        ThemePreference::Dark,
        ThemePreference::System,
    ];
    const SYSTEMS: [Appearance; 2] = [Appearance::Light, Appearance::Dark];

    #[test]
    fn resolve_all_combinations() {
        for system in SYSTEMS {
            assert_eq!(resolve(ThemePreference::Light, system), Appearance::Light);
            assert_eq!(resolve(ThemePreference::Dark, system), Appearance::Dark);
            assert_eq!(resolve(ThemePreference::System, system), system);
        }
    }

    #[test]
    fn toggled_flips_and_is_explicit() {
        for pref in PREFS {
            for system in SYSTEMS {
                let t = toggled(pref, system);
                assert_ne!(t, ThemePreference::System);
                assert_ne!(resolve(t, system), resolve(pref, system));
            }
        }
    }

    #[test]
    fn default_is_light() {
        assert_eq!(Appearance::default(), Appearance::Light);
    }
}
