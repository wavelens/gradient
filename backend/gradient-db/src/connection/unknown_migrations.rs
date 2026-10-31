/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Result;
use std::collections::HashSet;

pub(super) fn require_known_migrations(applied: &HashSet<String>, known: &[String]) -> Result<()> {
    let unknown = unknown_migrations(applied, known);
    if unknown.is_empty() {
        return Ok(());
    }

    anyhow::bail!(
        "the database has migrations applied that this binary does not know: {}. This binary is \
         older than the database or was built from another branch. Start the release that \
         applied them instead",
        unknown.join(", "),
    )
}

/// Migrations that an earlier major squashed into the baseline left rows sorting before it.
fn unknown_migrations<'a>(applied: &'a HashSet<String>, known: &[String]) -> Vec<&'a str> {
    let Some(baseline) = known.first() else {
        return Vec::new();
    };

    let mut unknown: Vec<&str> = applied
        .iter()
        .filter(|name| name.as_str() > baseline.as_str() && !known.contains(name))
        .map(String::as_str)
        .collect();
    unknown.sort_unstable();

    unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known() -> Vec<String> {
        ["m4_baseline", "m5_index", "m7_column"]
            .map(str::to_owned)
            .to_vec()
    }

    fn set(names: &[&str]) -> HashSet<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn a_database_at_or_behind_this_binary_is_accepted() {
        assert!(require_known_migrations(&set(&[]), &known()).is_ok());
        assert!(require_known_migrations(&set(&["m4_baseline", "m5_index"]), &known()).is_ok());
        assert!(
            require_known_migrations(&set(&["m4_baseline", "m5_index", "m7_column"]), &known())
                .is_ok()
        );
    }

    #[test]
    fn migrations_squashed_into_the_baseline_are_not_unknown() {
        let applied = set(&["m1_users", "m3_last", "m4_baseline"]);

        assert!(require_known_migrations(&applied, &known()).is_ok());
    }

    #[test]
    fn newer_and_foreign_branch_migrations_are_refused_by_name() {
        let applied = set(&[
            "m4_baseline",
            "m5_index",
            "m6_branch",
            "m7_column",
            "m9_newer",
        ]);

        let err = require_known_migrations(&applied, &known())
            .unwrap_err()
            .to_string();

        assert!(err.contains("m6_branch, m9_newer"), "{err}");
        assert!(!err.contains("m5_index"), "{err}");
        assert!(err.contains("older than the database"), "{err}");
    }
}
