// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Component detection.

/// Preamble of a component: magic `\0asm` and the component layer/version `0d 00 01 00`.
const COMPONENT_PREAMBLE: [u8; 8] = [0x00, 0x61, 0x73, 0x6d, 0x0d, 0x00, 0x01, 0x00];

/// Whether `bytes` start with the component preamble (a core module is rejected).
pub fn is_component(bytes: &[u8]) -> bool {
    bytes.starts_with(&COMPONENT_PREAMBLE)
}

#[cfg(test)]
mod tests {
    use super::is_component;

    #[test]
    fn accepts_component_preamble() {
        assert!(is_component(&[
            0x00, 0x61, 0x73, 0x6d, 0x0d, 0x00, 0x01, 0x00
        ]));
        assert!(is_component(&[
            0x00, 0x61, 0x73, 0x6d, 0x0d, 0x00, 0x01, 0x00, 0xff
        ]));
    }

    #[test]
    fn rejects_core_module_and_short_input() {
        assert!(!is_component(&[
            0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00
        ]));
        assert!(!is_component(&[]));
        assert!(!is_component(&[0x00, 0x61, 0x73, 0x6d, 0x0d, 0x00, 0x01]));
    }
}
