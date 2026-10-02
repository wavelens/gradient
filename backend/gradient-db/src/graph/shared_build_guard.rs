/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! A seed and a concurrent flip are each missing the other's uncommitted rows under READ COMMITTED.
//! The count would then stay one too high for good.
//! A flip is holding its shared build's key exclusively until commit.
//! A seed is holding its dependencies' keys shared, and the second one is waiting for the first.

pub const SHARED_BUILD_LOCK_NAMESPACE: i32 = 643;

pub(crate) fn advisory_filter(
    shared_builds_param: &str,
    with_dependencies: bool,
) -> (String, String) {
    let dependencies = if with_dependencies {
        format!(
            " UNION ALL SELECT hashtext(e.dependency::text), false \
             FROM derivation_dependency e WHERE e.derivation = ANY({shared_builds_param}::uuid[])"
        )
    } else {
        String::new()
    };

    key_pass(&format!(
        "SELECT hashtext(a::text) AS k, true AS own \
         FROM unnest({shared_builds_param}::uuid[]) AS a{dependencies}"
    ))
}

pub(crate) fn producer_filter(hashes_param: &str) -> (String, String) {
    key_pass(&format!(
        "SELECT hashtext(o.derivation::text) AS k, true AS own \
         FROM derivation_output o WHERE o.hash = ANY({hashes_param})"
    ))
}

fn key_pass(keys: &str) -> (String, String) {
    let with = format!(
        "shared_build_keys AS MATERIALIZED (SELECT k, bool_or(own) AS own FROM ({keys}) x \
         GROUP BY k ORDER BY k), \
         shared_build_locks AS (SELECT count(CASE WHEN own \
             THEN pg_advisory_xact_lock({ns}, k) \
             ELSE pg_advisory_xact_lock_shared({ns}, k) END) AS n FROM shared_build_keys)",
        ns = SHARED_BUILD_LOCK_NAMESPACE,
    );

    (with, "(SELECT n FROM shared_build_locks) >= 0".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lock_pass_is_sorted_materialised_and_grouped_by_key() {
        let (with, pred) = advisory_filter("$1", true);
        assert!(
            with.starts_with("shared_build_keys AS MATERIALIZED ("),
            "{with}"
        );
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
        assert_eq!(pred, "(SELECT n FROM shared_build_locks) >= 0");
    }

    #[test]
    fn without_dependencies_only_the_shared_builds_are_locked() {
        let (with, _) = advisory_filter("$1", false);
        assert!(!with.contains("derivation_dependency"), "{with}");
    }
}
