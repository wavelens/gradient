/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The log transport can carry nix ANSI output double-escaped as a literal `\u001b`. Decoding is
//! restoring the real control bytes, mirroring the web log viewer.

use std::io::IsTerminal;

pub fn color_enabled() -> bool {
    std::io::stdout().is_terminal()
}

pub fn decode_escapes(s: &str) -> String {
    s.replace("\\u001b", "\u{1b}")
        .replace("\\n", "\n")
        .replace("\\t", "\t")
}

pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        if chars.peek() == Some(&'[') {
            chars.next();
        }
        while let Some(&n) = chars.peek() {
            chars.next();
            if n.is_ascii_alphabetic() {
                break;
            }
        }
    }
    out
}

pub fn render_log(chunk: &str) -> String {
    let decoded = decode_escapes(chunk);
    if color_enabled() {
        decoded
    } else {
        strip_ansi(&decoded)
    }
}

pub fn eval_message_line(level: &str, message: &str) -> String {
    let (label, colour) = match level.to_ascii_lowercase().as_str() {
        "error" => ("error", "\u{1b}[1;31m"),
        "warning" => ("warning", "\u{1b}[1;33m"),
        _ => ("note", "\u{1b}[1;36m"),
    };
    if color_enabled() {
        format!("{colour}{label}:\u{1b}[0m {message}")
    } else {
        format!("{label}: {message}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_turns_literal_escape_into_real_esc() {
        assert_eq!(decode_escapes("a\\u001b[31mb"), "a\u{1b}[31mb");
    }

    #[test]
    fn strip_removes_color_sequences() {
        assert_eq!(strip_ansi("a\u{1b}[1;31mred\u{1b}[0mb"), "aredb");
    }

    #[test]
    fn message_line_labels_by_level() {
        assert!(eval_message_line("error", "boom").contains("error:"));
        assert!(eval_message_line("warning", "careful").contains("warning:"));
        assert!(eval_message_line("notice", "fyi").contains("note:"));
    }
}
