/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Post-processing for nix build-failure messages.

/// Remove nix's post-failure log tail - the `Last N log lines:` header and the
/// `>`-prefixed lines under it - from a build failure message.
///
/// Nix repeats the tail of the build log in the error it returns, so a failure
/// surfaced in the build log shows those lines twice: once as they were
/// streamed, once again inside the failure banner. Nix suppresses the tail
/// itself when `log-lines` is `0`, but the daemon only honours that setting
/// from a *trusted* client, so gradient cannot rely on it and strips the block
/// instead.
///
/// Everything else is preserved verbatim, including the `Cannot build`/`Reason`
/// lines above the tail and the `For full logs, run:` hint below it - with the
/// cache's log endpoint serving `nix log`, that hint now works.
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

/// Matches nix's `Last %d log lines:` header, which arrives indented when the
/// message is nested inside another error.
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

    /// The reported shape (#546): nix appends the tail of a log gradient has
    /// already streamed line by line, so the failure banner repeats it.
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

    /// Nested errors arrive indented, and any line count is possible since
    /// `log-lines` is configurable.
    #[test]
    fn strips_an_indented_header_with_any_line_count() {
        let message = "       Last 3 log lines:\n       > one\n       > two\n       done";
        assert_eq!(strip_nix_log_tail(message), "       done");
    }

    /// A quoted line that is part of the builder's own output, before any
    /// header, is diagnosis - not the tail block.
    #[test]
    fn keeps_quoted_lines_that_precede_a_header() {
        let message = "> not a tail line\nerror: build failed";
        assert_eq!(strip_nix_log_tail(message), message);
    }

    /// A message without a tail block is passed through untouched, trailing
    /// newline included.
    #[test]
    fn leaves_a_message_without_a_tail_untouched() {
        let message = "error: hash mismatch in fixed-output derivation\n";
        assert_eq!(strip_nix_log_tail(message), message);
    }

    /// A multi-build failure carries one block per failed build.
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
