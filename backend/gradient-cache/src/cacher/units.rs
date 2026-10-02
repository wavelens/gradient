/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

/// A checkpoint missing from a newer release's list is resuming at its successor.
pub(crate) fn next_unit<'a>(units: &'a [String], checkpoint: Option<&str>) -> Option<&'a str> {
    units
        .iter()
        .map(String::as_str)
        .find(|unit| checkpoint.is_none_or(|done| *unit > done))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    Idle,
    Paced,
    Requested,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn units() -> Vec<String> {
        ["logs/00", "logs/01", "nars/00"].map(String::from).to_vec()
    }

    #[test]
    fn resumes_past_the_checkpoint() {
        assert_eq!(next_unit(&units(), None), Some("logs/00"));
        assert_eq!(next_unit(&units(), Some("logs/01")), Some("nars/00"));
        assert_eq!(next_unit(&units(), Some("nars/00")), None);
    }

    #[test]
    fn a_checkpoint_no_longer_listed_resumes_at_its_successor() {
        assert_eq!(next_unit(&units(), Some("logs/00a")), Some("logs/01"));
    }
}
