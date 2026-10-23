// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Turns the JSON-with-comments of an editor settings file into plain JSON.
//!
//! Comments (`//` and `/* */`) and trailing commas are replaced by spaces, so line and column
//! numbers of later parse errors still point at the original text. Strings are tracked, so `//`
//! inside a string is kept. Two linear passes; no recursion; the output is never longer than the
//! input.

use crate::error::ImportError;

fn malformed(bytes: &[u8], offset: usize, message: &str) -> ImportError {
    let before = &bytes[..offset.min(bytes.len())];
    let line = before.split(|b| *b == b'\n').count();
    let column = before.iter().rev().take_while(|b| **b != b'\n').count() + 1;
    ImportError::Malformed {
        line,
        column,
        message: message.to_owned(),
    }
}

/// Blanks comments and trailing commas. The input must be valid UTF-8; the output then is too,
/// because only whole comments and ASCII commas are overwritten with ASCII spaces.
pub(super) fn to_json(bytes: &[u8]) -> Result<Vec<u8>, ImportError> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut in_string = false;
    let mut escaped = false;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if in_string {
            out.push(b);
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        match (b, bytes.get(i + 1)) {
            (b'"', _) => {
                in_string = true;
                out.push(b);
                i += 1;
            }
            (b'/', Some(b'/')) => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    out.push(b' ');
                    i += 1;
                }
            }
            (b'/', Some(b'*')) => {
                let start = i;
                out.extend_from_slice(b"  ");
                i += 2;
                loop {
                    match (bytes.get(i), bytes.get(i + 1)) {
                        (Some(b'*'), Some(b'/')) => {
                            out.extend_from_slice(b"  ");
                            i += 2;
                            break;
                        }
                        (Some(b'\n'), _) => {
                            out.push(b'\n');
                            i += 1;
                        }
                        (Some(_), _) => {
                            out.push(b' ');
                            i += 1;
                        }
                        (None, _) => {
                            return Err(malformed(bytes, start, "unterminated comment"));
                        }
                    }
                }
            }
            _ => {
                out.push(b);
                i += 1;
            }
        }
    }
    blank_trailing_commas(&mut out);
    Ok(out)
}

/// A comma whose next significant byte closes an array or object is replaced by a space.
fn blank_trailing_commas(buf: &mut [u8]) {
    let mut in_string = false;
    let mut escaped = false;
    for i in 0..buf.len() {
        let b = buf[i];
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        if b == b'"' {
            in_string = true;
        } else if b == b',' {
            let next = buf[i + 1..]
                .iter()
                .find(|c| !matches!(c, b' ' | b'\t' | b'\r' | b'\n'));
            if matches!(next, Some(b']' | b'}')) {
                buf[i] = b' ';
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(s: &str) -> String {
        String::from_utf8(to_json(s.as_bytes()).unwrap()).unwrap()
    }

    #[test]
    fn comments_and_trailing_commas_are_blanked() {
        let src = "{ // line\n \"a\": [1, 2, /* x */ ],\n /* multi\nline */ \"b\": 3, }";
        let v: serde_json::Value = serde_json::from_str(&run(src)).unwrap();
        assert_eq!(v["a"], serde_json::json!([1, 2]));
        assert_eq!(v["b"], 3);
        assert_eq!(run(src).len(), src.len());
        assert_eq!(
            run(src).matches('\n').count(),
            src.matches('\n').count(),
            "lines are kept"
        );
    }

    #[test]
    fn strings_are_left_alone() {
        let src = r#"{"u": "http://example.test/*not a comment*/", "q": "a\"//b,", "c": ",}"}"#;
        assert_eq!(run(src), src);
    }

    #[test]
    fn unterminated_comment_is_malformed_with_position() {
        let e = to_json(b"{\n  /* open").unwrap_err();
        assert_eq!(
            e,
            ImportError::Malformed {
                line: 2,
                column: 3,
                message: "unterminated comment".to_owned()
            }
        );
    }

    #[test]
    fn comma_before_a_value_is_kept() {
        assert_eq!(run("[1, // c\n 2]"), "[1,     \n 2]");
    }
}
