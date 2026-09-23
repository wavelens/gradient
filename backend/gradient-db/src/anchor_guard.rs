/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Advisory keys that serialise a dependency count against a flip of what it counts.
//!
//! A seed writes an absolute count over an anchor's dependencies, read from its
//! snapshot; a flip of one of those dependencies ripples over the edges into it, read
//! from its own. Under READ COMMITTED each misses the other's uncommitted rows: the
//! seed counts the dependency as a hole while the flip's ripple cannot see the edge
//! the seed just inserted, and the count stays one too high for good. The keys close
//! that. A flip holds its anchor's key exclusively from its lock until commit, and a
//! seed holds its dependencies' keys shared, so whichever comes second waits for the
//! first to commit and then reads its rows: the ripple sees the seed's edge, or the
//! seed sees the flip.
//!
//! Shared holders never conflict with each other and write nothing to the rows, so a
//! dependency every `.drv` references costs a lock-table entry per seed and not a
//! queue. The keys are taken in one pass sorted by key, inside the statement that
//! takes the row locks, as its `One-Time Filter`, so they precede every row lock and
//! add no statement. A key an anchor and a dependency share is taken exclusively once.

pub const ANCHOR_LOCK_NAMESPACE: i32 = 643;

/// The `WITH` members (without the keyword) that take the keys of the anchors bound
/// at `anchors_param`, and of their dependencies when `with_dependencies`, and the
/// predicate that forces them to run before the statement reads a row.
pub(crate) fn advisory_filter(anchors_param: &str, with_dependencies: bool) -> (String, String) {
    let dependencies = if with_dependencies {
        format!(
            " UNION ALL SELECT hashtext(e.dependency::text), false \
             FROM derivation_dependency e WHERE e.derivation = ANY({anchors_param}::uuid[])"
        )
    } else {
        String::new()
    };

    key_pass(&format!(
        "SELECT hashtext(a::text) AS k, true AS own \
         FROM unnest({anchors_param}::uuid[]) AS a{dependencies}"
    ))
}

/// [`advisory_filter`] for the producers of the store paths bound at `hashes_param`,
/// exclusively: what a retire takes before it reads whether they were whole.
pub(crate) fn producer_filter(hashes_param: &str) -> (String, String) {
    key_pass(&format!(
        "SELECT hashtext(o.derivation::text) AS k, true AS own \
         FROM derivation_output o WHERE o.hash = ANY({hashes_param})"
    ))
}

fn key_pass(keys: &str) -> (String, String) {
    let with = format!(
        "anchor_keys AS MATERIALIZED (SELECT k, bool_or(own) AS own FROM ({keys}) x \
         GROUP BY k ORDER BY k), \
         anchor_locks AS (SELECT count(CASE WHEN own \
             THEN pg_advisory_xact_lock({ns}, k) \
             ELSE pg_advisory_xact_lock_shared({ns}, k) END) AS n FROM anchor_keys)",
        ns = ANCHOR_LOCK_NAMESPACE,
    );

    (with, "(SELECT n FROM anchor_locks) >= 0".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lock_pass_is_sorted_materialised_and_grouped_by_key() {
        let (with, pred) = advisory_filter("$1", true);
        assert!(with.starts_with("anchor_keys AS MATERIALIZED ("), "{with}");
        assert!(with.contains("bool_or(own) AS own"), "{with}");
        assert!(with.contains("GROUP BY k ORDER BY k"), "{with}");
        assert!(
            with.contains("FROM derivation_dependency e WHERE e.derivation = ANY($1::uuid[])"),
            "{with}"
        );
        assert!(with.contains("pg_advisory_xact_lock(643, k)"), "{with}");
        assert!(
            with.contains("pg_advisory_xact_lock_shared(643, k)"),
            "{with}"
        );
        assert_eq!(pred, "(SELECT n FROM anchor_locks) >= 0");
    }

    #[test]
    fn without_dependencies_only_the_anchors_are_keyed() {
        let (with, _) = advisory_filter("$1", false);
        assert!(!with.contains("derivation_dependency"), "{with}");
    }
}
