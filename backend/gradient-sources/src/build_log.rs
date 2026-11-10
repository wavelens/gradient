/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

/// Nix is repeating the streamed log tail inside the failure banner. The daemon is honouring
/// `log-lines = 0` only from a trusted client, and gradient must strip the block itself.
pub fn strip_nix_log_tail(message: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut in_tail = false;

    for line in message.lines() {
        if is_log_tail_header(line) {
            in_tail = true;
            continue;
        }
        if in_tail {
            if line.trim_start().starts_with('>') {
                continue;
            }
            in_tail = false;
        }
        out.push(line);
    }

    rejoin(message, without_nix_log_pointer(out))
}

pub fn replace_nix_log_pointer_with_web_log_link(log: &str, link: &str) -> String {
    let mut replaced = rejoin(log, without_nix_log_pointer(log.lines().collect()));
    if !replaced.is_empty() && !replaced.ends_with('\n') {
        replaced.push('\n');
    }
    replaced.push_str(&format!("Full log: {link}\n"));
    replaced
}

pub fn strip_ansi_escape_sequences(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('[') => {
                for c in chars.by_ref() {
                    if ('\x40'..='\x7e').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\x07' || (c == '\x1b' && chars.next_if_eq(&'\\').is_some()) {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

fn without_nix_log_pointer(lines: Vec<&str>) -> Vec<&str> {
    let mut out = Vec::with_capacity(lines.len());
    let mut lines = lines.into_iter().peekable();
    while let Some(line) = lines.next() {
        let is_pointer = strip_ansi_escape_sequences(line).trim() == "For full logs, run:"
            && lines.peek().is_some_and(|next| {
                strip_ansi_escape_sequences(next)
                    .trim()
                    .starts_with("nix log ")
            });
        if is_pointer {
            lines.next();
            continue;
        }
        out.push(line);
    }
    out
}

fn rejoin(original: &str, lines: Vec<&str>) -> String {
    let mut joined = lines.join("\n");
    if original.ends_with('\n') && !joined.is_empty() {
        joined.push('\n');
    }
    joined
}

fn is_log_tail_header(line: &str) -> bool {
    let line = line.trim();
    let Some(rest) = line.strip_prefix("Last ") else {
        return false;
    };
    let Some(count) = rest.strip_suffix(" log lines:") else {
        return false;
    };

    !count.is_empty() && count.chars().all(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_the_tail_block_and_the_server_only_nix_log_pointer() {
        let message = "\
Cannot build '/nix/store/1mpqffikzpszxw6zzi8s63a3srqd6swx-python3.14-ctranslate2-4.8.1.drv'.
Reason: builder failed with exit code 1.
Output paths:
  /nix/store/d6ynvin99dw364i198n0k0jsfn0z53wh-python3.14-ctranslate2-4.8.1-dist
Last 25 log lines:
> running build_ext
> building 'ctranslate2._ext' extension
For full logs, run:
  nix log /nix/store/1mpqffikzpszxw6zzi8s63a3srqd6swx-python3.14-ctranslate2-4.8.1.drv";

        assert_eq!(
            strip_nix_log_tail(message),
            "\
Cannot build '/nix/store/1mpqffikzpszxw6zzi8s63a3srqd6swx-python3.14-ctranslate2-4.8.1.drv'.
Reason: builder failed with exit code 1.
Output paths:
  /nix/store/d6ynvin99dw364i198n0k0jsfn0z53wh-python3.14-ctranslate2-4.8.1-dist"
        );
    }

    #[test]
    fn replaces_a_coloured_nix_log_pointer_with_the_web_link() {
        let log = "building\nerror: boom\nFor full logs, run:\n  \x1b[35;1mnix log /nix/store/x-fail.drv\x1b[0m\n";
        assert_eq!(
            replace_nix_log_pointer_with_web_log_link(log, "https://ci/project/p/log/e?build=b"),
            "building\nerror: boom\nFull log: https://ci/project/p/log/e?build=b\n"
        );
    }

    #[test]
    fn appends_the_web_link_to_a_log_without_pointer_or_final_newline() {
        assert_eq!(
            replace_nix_log_pointer_with_web_log_link("error: boom", "u"),
            "error: boom\nFull log: u\n"
        );
        assert_eq!(
            replace_nix_log_pointer_with_web_log_link("", "u"),
            "Full log: u\n"
        );
    }

    #[test]
    fn strips_colour_and_title_escape_sequences() {
        assert_eq!(
            strip_ansi_escape_sequences(
                "\x1b[31;1merror:\x1b[0m bad \x1b]0;title\x07ok \x1b]8;;u\x1b\\link\x1b[K"
            ),
            "error: bad ok link"
        );
    }

    #[test]
    fn strips_an_indented_header_with_any_line_count() {
        let message = "       Last 3 log lines:\n       > one\n       > two\n       done";
        assert_eq!(strip_nix_log_tail(message), "       done");
    }

    #[test]
    fn keeps_quoted_lines_that_precede_a_header() {
        let message = "> not a tail line\nerror: build failed";
        assert_eq!(strip_nix_log_tail(message), message);
    }

    #[test]
    fn leaves_a_message_without_a_tail_untouched() {
        let message = "error: hash mismatch in fixed-output derivation\n";
        assert_eq!(strip_nix_log_tail(message), message);
    }

    #[test]
    fn strips_every_block_in_a_multi_build_failure() {
        let message = "\
first failure
Last 2 log lines:
> a
> b
second failure
Last 1 log lines:
> c
tail end";
        assert_eq!(
            strip_nix_log_tail(message),
            "first failure\nsecond failure\ntail end"
        );
    }
}
