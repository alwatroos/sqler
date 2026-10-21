// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The static parts of the package, XML escaping and column letters.

use std::fmt::Write as _;

/// Style indexes into `cellXfs` of [`STYLES`].
pub(super) const STYLE_HEADER: u8 = 1;
pub(super) const STYLE_DATE: u8 = 2;
pub(super) const STYLE_DATETIME: u8 = 3;
pub(super) const STYLE_TIME: u8 = 4;
pub(super) const STYLE_QUOTE: u8 = 5;
pub(super) const STYLE_HEADER_QUOTE: u8 = 6;

const DECLARATION: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#;

pub(super) const CONTENT_TYPES: &str = concat!(
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
    r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">"#,
    r#"<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>"#,
    r#"<Default Extension="xml" ContentType="application/xml"/>"#,
    r#"<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>"#,
    r#"<Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>"#,
    r#"<Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/>"#,
    "</Types>"
);

pub(super) const ROOT_RELS: &str = concat!(
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
    r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
    r#"<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>"#,
    "</Relationships>"
);

/// The workbook has one sheet, named `Sheet1`. The name follows Excel's rules: 1 to 31
/// characters, none of `\ / ? * [ ] :`, no leading or trailing apostrophe.
pub(super) const WORKBOOK: &str = concat!(
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
    r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">"#,
    r#"<sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets>"#,
    "</workbook>"
);

pub(super) const WORKBOOK_RELS: &str = concat!(
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
    r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
    r#"<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>"#,
    r#"<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>"#,
    "</Relationships>"
);

/// Two fonts (normal, bold), the two mandatory fills, one border, number formats 164 to 166
/// and the cell formats that the `STYLE_*` constants index.
pub(super) const STYLES: &str = concat!(
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
    r#"<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">"#,
    r#"<numFmts count="3">"#,
    r#"<numFmt numFmtId="164" formatCode="yyyy\-mm\-dd"/>"#,
    r#"<numFmt numFmtId="165" formatCode="yyyy\-mm\-dd\ hh:mm:ss"/>"#,
    r#"<numFmt numFmtId="166" formatCode="hh:mm:ss"/>"#,
    "</numFmts>",
    r#"<fonts count="2">"#,
    r#"<font><sz val="11"/><name val="Calibri"/><family val="2"/></font>"#,
    r#"<font><b/><sz val="11"/><name val="Calibri"/><family val="2"/></font>"#,
    "</fonts>",
    r#"<fills count="2"><fill><patternFill patternType="none"/></fill><fill><patternFill patternType="gray125"/></fill></fills>"#,
    r#"<borders count="1"><border><left/><right/><top/><bottom/><diagonal/></border></borders>"#,
    r#"<cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs>"#,
    r#"<cellXfs count="7">"#,
    r#"<xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0"/>"#,
    r#"<xf numFmtId="0" fontId="1" fillId="0" borderId="0" xfId="0" applyFont="1"/>"#,
    r#"<xf numFmtId="164" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/>"#,
    r#"<xf numFmtId="165" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/>"#,
    r#"<xf numFmtId="166" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/>"#,
    r#"<xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0" quotePrefix="1"/>"#,
    r#"<xf numFmtId="0" fontId="1" fillId="0" borderId="0" xfId="0" applyFont="1" quotePrefix="1"/>"#,
    "</cellXfs>",
    r#"<cellStyles count="1"><cellStyle name="Normal" xfId="0" builtinId="0"/></cellStyles>"#,
    "</styleSheet>"
);

/// The start of the sheet part, up to and including `<sheetData>`.
pub(super) fn sheet_start() -> String {
    format!(
        "{DECLARATION}<worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"><sheetData>"
    )
}

/// The end of the sheet part.
pub(super) const SHEET_END: &str = "</sheetData></worksheet>";

/// The column letters for a zero-based index: 0 is `A`, 25 is `Z`, 26 is `AA`, 16383 is `XFD`.
pub(super) fn column_letters(mut index: usize) -> String {
    let mut letters = Vec::new();
    loop {
        letters.push(b'A' + u8::try_from(index % 26).unwrap_or(0));
        if index < 26 {
            break;
        }
        index = index / 26 - 1;
    }
    letters.reverse();
    String::from_utf8(letters).unwrap_or_default()
}

/// Whether `s` starts a `_xHHHH_` escape.
fn is_escape_at(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 7
        && b[0] == b'_'
        && b[1] == b'x'
        && b[6] == b'_'
        && b[2..6].iter().all(u8::is_ascii_hexdigit)
}

/// Appends `text` as XML character data.
///
/// `&`, `<`, `>` and `"` become entities. A carriage return becomes `&#13;` because a parser
/// would otherwise turn it into a line feed. Control characters that XML 1.0 forbids (every
/// one below U+0020 except tab, line feed and carriage return) and U+FFFE and U+FFFF are
/// written as `_xHHHH_`, the escape Excel reads back as that character. Text that already
/// looks like such an escape gets its underscore written as `_x005F_`, so it reads back
/// unchanged.
pub(super) fn push_escaped(out: &mut String, text: &str) {
    for (i, c) in text.char_indices() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\r' => out.push_str("&#13;"),
            '\t' | '\n' => out.push(c),
            '_' if is_escape_at(&text[i..]) => out.push_str("_x005F_"),
            c if c < ' ' || c == '\u{FFFE}' || c == '\u{FFFF}' => {
                let _ = write!(out, "_x{:04X}_", u32::from(c));
            }
            c => out.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn esc(s: &str) -> String {
        let mut out = String::new();
        push_escaped(&mut out, s);
        out
    }

    #[test]
    fn letters() {
        assert_eq!(column_letters(0), "A");
        assert_eq!(column_letters(25), "Z");
        assert_eq!(column_letters(26), "AA");
        assert_eq!(column_letters(51), "AZ");
        assert_eq!(column_letters(52), "BA");
        assert_eq!(column_letters(701), "ZZ");
        assert_eq!(column_letters(702), "AAA");
        assert_eq!(column_letters(16_383), "XFD");
    }

    #[test]
    fn entities_and_whitespace() {
        assert_eq!(esc("a&b<c>d\"e'f"), "a&amp;b&lt;c&gt;d&quot;e'f");
        assert_eq!(esc("a\tb\nc\rd"), "a\tb\nc&#13;d");
    }

    #[test]
    fn xml_forbidden_characters_are_escaped() {
        assert_eq!(esc("a\u{0}b\u{1b}c\u{1f}"), "a_x0000_b_x001B_c_x001F_");
        assert_eq!(esc("\u{FFFE}\u{FFFF}"), "_xFFFE__xFFFF_");
        assert_eq!(esc("\u{7f}\u{85}é😀"), "\u{7f}\u{85}é😀");
    }

    #[test]
    fn literal_escapes_are_protected() {
        assert_eq!(esc("_x0041_"), "_x005F_x0041_");
        assert_eq!(esc("_xZZZZ_ _x41_ x_x0041"), "_xZZZZ_ _x41_ x_x0041");
        assert_eq!(esc("x_x00aB_"), "x_x005F_x00aB_");
        assert_eq!(esc("snake_case_x"), "snake_case_x");
    }

    #[test]
    fn static_parts_are_well_formed_and_consistent() {
        for part in [CONTENT_TYPES, ROOT_RELS, WORKBOOK, WORKBOOK_RELS, STYLES] {
            assert!(part.starts_with("<?xml"));
        }
        assert!(WORKBOOK.contains("name=\"Sheet1\""));
        assert_eq!(STYLES.matches("<xf ").count(), 1 + 7);
        assert!(STYLES.contains("quotePrefix=\"1\""));
        assert!(sheet_start().ends_with("<sheetData>"));
    }
}
