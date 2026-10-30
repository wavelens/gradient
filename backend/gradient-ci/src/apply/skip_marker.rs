/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

const SKIP_MARKERS: [&str; 5] = [
    "[skip ci]",
    "[ci skip]",
    "[no ci]",
    "[skip gradient]",
    "[gradient skip]",
];

pub(super) fn commit_requests_skip(message: &str) -> bool {
    let lowered = message.to_lowercase();
    SKIP_MARKERS.iter().any(|marker| lowered.contains(marker)) || has_skip_checks_trailer(message)
}

fn has_skip_checks_trailer(message: &str) -> bool {
    let Some((_, trailers)) = message.trim_end().rsplit_once("\n\n") else {
        return false;
    };
    trailers.lines().any(|line| {
        line.split_once(':').is_some_and(|(key, value)| {
            key.trim().eq_ignore_ascii_case("skip-checks")
                && value.trim().eq_ignore_ascii_case("true")
        })
    })
}

#[cfg(test)]
mod tests {
    use super::commit_requests_skip;

    #[test]
    fn a_marker_anywhere_in_the_message_asks_to_skip_regardless_of_case() {
        assert!(commit_requests_skip("docs: fix typo [skip ci]"));
        assert!(commit_requests_skip("[CI SKIP] docs: fix typo"));
        assert!(commit_requests_skip(
            "docs: fix typo\n\nOnly prose, [No Ci]."
        ));
        assert!(commit_requests_skip("chore: bump [Skip Gradient]"));
    }

    #[test]
    fn a_skip_checks_trailer_asks_to_skip() {
        assert!(commit_requests_skip(
            "docs: fix typo\n\nSigned-off-by: A <a@example.invalid>\nSkip-Checks: true\n"
        ));
        assert!(commit_requests_skip("docs: fix typo\n\nskip-checks:true"));
    }

    #[test]
    fn a_message_without_a_marker_does_not_ask_to_skip() {
        assert!(!commit_requests_skip("feat: skip ci on docs-only changes"));
        assert!(!commit_requests_skip("fix: [skip] the flaky test"));
        assert!(!commit_requests_skip(""));
    }

    #[test]
    fn a_skip_checks_line_outside_the_trailer_block_does_not_ask_to_skip() {
        assert!(!commit_requests_skip("skip-checks: true"));
        assert!(!commit_requests_skip(
            "docs: explain\n\nskip-checks: true\n\nSigned-off-by: A <a@example.invalid>"
        ));
        assert!(!commit_requests_skip(
            "docs: fix typo\n\nskip-checks: false"
        ));
    }
}
