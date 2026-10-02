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

    let mut stripped = out.join("\n");
    if message.ends_with('\n') && !stripped.is_empty() {
        stripped.push('\n');
    }

    stripped
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
    fn strips_the_tail_block_and_keeps_the_diagnosis() {
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
  /nix/store/d6ynvin99dw364i198n0k0jsfn0z53wh-python3.14-ctranslate2-4.8.1-dist
For full logs, run:
  nix log /nix/store/1mpqffikzpszxw6zzi8s63a3srqd6swx-python3.14-ctranslate2-4.8.1.drv"
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
