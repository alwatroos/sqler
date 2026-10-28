// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Writers for the ICO and ICNS containers; every entry is an embedded PNG.

/// Append a little-endian `u16`.
fn push_u16_le(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

/// Append a little-endian `u32`.
fn push_u32_le(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

/// Length as `u32`; icons are far below 4 GiB, so saturating never happens in practice.
fn len_u32(len: usize) -> u32 {
    u32::try_from(len).unwrap_or(u32::MAX)
}

/// An ICO file from `(size, png bytes)` entries in ascending size order. Sizes of 256 are encoded as 0.
pub(crate) fn write_ico(entries: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    push_u16_le(&mut out, 0);
    push_u16_le(&mut out, 1);
    push_u16_le(&mut out, u16::try_from(entries.len()).unwrap_or(u16::MAX));
    let mut offset = 6 + 16 * entries.len();
    for (size, png) in entries {
        let dim = u8::try_from(*size).unwrap_or(0);
        out.extend_from_slice(&[dim, dim, 0, 0]);
        push_u16_le(&mut out, 1);
        push_u16_le(&mut out, 32);
        push_u32_le(&mut out, len_u32(png.len()));
        push_u32_le(&mut out, len_u32(offset));
        offset += png.len();
    }
    for (_, png) in entries {
        out.extend_from_slice(png);
    }
    out
}

/// An ICNS file from `(OSType, png bytes)` entries, big-endian lengths.
pub(crate) fn write_icns(entries: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
    let total = 8 + entries.iter().map(|(_, p)| 8 + p.len()).sum::<usize>();
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"icns");
    out.extend_from_slice(&len_u32(total).to_be_bytes());
    for (kind, png) in entries {
        out.extend_from_slice(kind);
        out.extend_from_slice(&len_u32(8 + png.len()).to_be_bytes());
        out.extend_from_slice(png);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ico_layout_with_two_entries() {
        let ico = write_ico(&[(16, vec![1, 2, 3]), (256, vec![9; 5])]);
        assert_eq!(&ico[..6], &[0, 0, 1, 0, 2, 0]);
        assert_eq!(&ico[6..10], &[16, 16, 0, 0]);
        assert_eq!(&ico[10..14], &[1, 0, 32, 0]);
        assert_eq!(u32::from_le_bytes([ico[14], ico[15], ico[16], ico[17]]), 3);
        assert_eq!(u32::from_le_bytes([ico[18], ico[19], ico[20], ico[21]]), 38);
        assert_eq!(&ico[22..26], &[0, 0, 0, 0]);
        assert_eq!(u32::from_le_bytes([ico[30], ico[31], ico[32], ico[33]]), 5);
        assert_eq!(u32::from_le_bytes([ico[34], ico[35], ico[36], ico[37]]), 41);
        assert_eq!(&ico[38..41], &[1, 2, 3]);
        assert_eq!(&ico[41..], &[9; 5]);
    }

    #[test]
    fn icns_layout() {
        let icns = write_icns(&[(*b"icp4", vec![1, 2]), (*b"ic07", vec![3])]);
        assert_eq!(&icns[..4], b"icns");
        assert_eq!(
            u32::from_be_bytes([icns[4], icns[5], icns[6], icns[7]]),
            8 + 10 + 9
        );
        assert_eq!(icns.len(), 27);
        assert_eq!(&icns[8..12], b"icp4");
        assert_eq!(
            u32::from_be_bytes([icns[12], icns[13], icns[14], icns[15]]),
            10
        );
        assert_eq!(&icns[16..18], &[1, 2]);
        assert_eq!(&icns[18..22], b"ic07");
        assert_eq!(
            u32::from_be_bytes([icns[22], icns[23], icns[24], icns[25]]),
            9
        );
        assert_eq!(icns[26], 3);
    }
}
