/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::build::BuildStatus;

pub fn drv_present_predicate(alias: &str) -> String {
    format!(
        "EXISTS (SELECT 1 FROM derivation d JOIN cached_path cp ON cp.hash = d.hash \
         WHERE d.id = {alias}.derivation AND cp.file_hash IS NOT NULL)"
    )
}

pub fn drv_nar_absent_predicate(alias: &str) -> String {
    format!("NOT {}", drv_present_predicate(alias))
}

pub fn walked_predicate(alias: &str) -> String {
    format!("EXISTS (SELECT 1 FROM derivation w WHERE w.id = {alias}.derivation AND w.walked)")
}

pub const BUILDER_STATUSES: [BuildStatus; 4] = [
    BuildStatus::Created,
    BuildStatus::Queued,
    BuildStatus::Building,
    BuildStatus::FailedTransient,
];

pub const NEED_BUILD_STATUSES: [BuildStatus; 6] = [
    BuildStatus::Created,
    BuildStatus::Queued,
    BuildStatus::Building,
    BuildStatus::FailedTransient,
    BuildStatus::Skipped,
    BuildStatus::Aborted,
];

pub fn blocks_evaluation(status: BuildStatus, wanted: bool) -> bool {
    if !NEED_BUILD_STATUSES.contains(&status) {
        return false;
    }

    wanted || matches!(status, BuildStatus::Queued | BuildStatus::Building)
}

pub fn blocks_evaluation_predicate(alias: &str) -> String {
    format!(
        "({alias}.status IN ({pending}) AND ({alias}.wanted OR {alias}.status IN ({in_flight})))",
        pending = crate::sql::status::build_in(&NEED_BUILD_STATUSES),
        in_flight = crate::sql::status::build_in(&[BuildStatus::Queued, BuildStatus::Building]),
    )
}

pub fn builder_predicate(shared_build: &str, walked: &str) -> String {
    format!(
        "{walked}.walked AND {shared_build}.probed AND NOT {shared_build}.cache_available \
         AND {shared_build}.status IN ({pending})",
        pending = crate::sql::status::build_in(&BUILDER_STATUSES),
    )
}

pub fn open_predicate(alias: &str) -> String {
    format!(
        "(NOT {alias}.fetchable AND {alias}.status NOT IN ({failed}))",
        failed = crate::sql::status::build_in(&BuildStatus::TERMINAL_FAILURE),
    )
}

pub fn non_passthrough_predicate(derivation: &str) -> String {
    format!(
        "NOT EXISTS (SELECT 1 FROM derivation_build rb \
         WHERE rb.derivation = {derivation} AND rb.cache_available)"
    )
}

/// The `EXISTS` over `derivation_output` is load-bearing.
/// The `NOT EXISTS` is vacuously true for a shared build with no output rows.
/// Its parents would be dispatched against an input nothing can provide without the guard.
pub fn fetchable_predicate(alias: &str) -> String {
    format!(
        "({alias}.status IN ({terminal_success}) AND {complete})",
        terminal_success = crate::sql::status::build_in(&BuildStatus::TERMINAL_SUCCESS),
        complete = shared_build_complete_predicate(alias),
    )
}

pub fn present_predicate(alias: &str) -> String {
    format!(
        "(EXISTS (SELECT 1 FROM derivation_output o2 WHERE o2.derivation = {alias}.derivation) \
         AND NOT EXISTS (SELECT 1 FROM derivation_output o LEFT JOIN cached_path cp ON cp.hash = o.hash \
                         WHERE o.derivation = {alias}.derivation AND cp.file_hash IS NULL))"
    )
}

pub fn present_value(alias: &str) -> String {
    format!(
        "coalesce((SELECT bool_and(cp.file_hash IS NOT NULL) FROM derivation_output o \
                   LEFT JOIN cached_path cp ON cp.hash = o.hash \
                   WHERE o.derivation = {alias}.derivation), false)"
    )
}

pub fn shared_build_complete_predicate(alias: &str) -> String {
    format!(
        "({alias}.missing_runtime_deps = 0 AND {present})",
        present = present_predicate(alias),
    )
}

/// The gates must never read `{alias}`'s own `status`.
/// The can-start migrations are running a demote and a promote in one transaction.
/// They are only independent while this predicate is ignoring the column the demote is writing.
pub fn gates_predicate(alias: &str) -> String {
    format!(
        r#"({walked}
    AND EXISTS (SELECT 1 FROM build_job bj WHERE bj.derivation = {alias}.derivation)
    AND {alias}.wanted
    AND ({alias}.cache_available
         OR ({alias}.probed AND {alias}.blocking_deps = 0 AND {drv_present})))"#,
        walked = walked_predicate(alias),
        drv_present = drv_present_predicate(alias),
    )
}

pub fn promotable_predicate(alias: &str) -> String {
    format!(
        "({alias}.status = {created} AND {gates})",
        created = crate::sql::status::build(BuildStatus::Created),
        gates = gates_predicate(alias),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::walks::open_closure_cte;

    fn norm(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn only_wanted_or_in_flight_work_blocks_an_evaluation() {
        use BuildStatus::*;

        for status in BUILDER_STATUSES {
            assert!(
                blocks_evaluation(status, true),
                "{status:?} is pending work something wants"
            );
        }
        for status in [Queued, Building] {
            assert!(
                blocks_evaluation(status, false),
                "{status:?} is handed out on its status alone, wanted or not"
            );
        }
        for status in [Created, FailedTransient, Skipped, Aborted] {
            assert!(
                !blocks_evaluation(status, false),
                "{status:?} with nothing wanting it is work that never happens"
            );
        }
        assert!(
            blocks_evaluation(Skipped, true),
            "a skipped shared build that is wanted again is owed work: the need is \
             written before the thaw, and an evaluation settled in that gap is \
             settled over a subtree it is about to queue"
        );
        for status in [Completed, Substituted, FailedPermanent, DependencyFailed] {
            assert!(!blocks_evaluation(status, true), "{status:?} is settled");
        }
    }

    #[test]
    fn the_predicate_names_what_blocks_evaluation_names() {
        use BuildStatus::*;

        let sql = blocks_evaluation_predicate("db");
        assert_eq!(
            sql,
            format!(
                "(db.status IN ({pending}) AND (db.wanted OR db.status IN ({in_flight})))",
                pending = crate::sql::status::build_in(&NEED_BUILD_STATUSES),
                in_flight = crate::sql::status::build_in(&[Queued, Building]),
            ),
            "the predicate is the Rust rule written out; changing one without the \
             other settles an evaluation whose builds are still running",
        );
    }

    #[test]
    fn the_gate_needs_the_drv_present_not_complete() {
        let g = norm(&gates_predicate("db"));
        assert!(
            g.contains("cp.file_hash IS NOT NULL") && !g.contains("missing_references"),
            "{g}"
        );
        assert_eq!(
            norm(&drv_present_predicate("db")),
            norm(&drv_nar_absent_predicate("db")).trim_start_matches("NOT "),
            "the two must stay exact negations"
        );
    }

    #[test]
    fn complete_is_present_with_no_missing_runtime_dep_and_fetchable_reads_it() {
        assert_eq!(
            norm(&shared_build_complete_predicate("db")),
            "(db.missing_runtime_deps = 0 AND (EXISTS (SELECT 1 FROM derivation_output o2 \
             WHERE o2.derivation = db.derivation) AND NOT EXISTS (SELECT 1 FROM derivation_output o \
             LEFT JOIN cached_path cp ON cp.hash = o.hash WHERE o.derivation = db.derivation \
             AND cp.file_hash IS NULL)))"
        );
        let f = norm(&fetchable_predicate("db"));
        assert!(f.contains("db.missing_runtime_deps = 0"), "{f}");
        assert!(
            !f.contains("missing_references"),
            "the path counter is no longer read: {f}"
        );
    }

    #[test]
    fn fetchable_is_terminal_success_with_complete_outputs_in_our_own_cache() {
        let p = norm(&fetchable_predicate("db"));
        assert!(p.starts_with("(db.status IN (3, 7)"), "{p}");
        assert!(p.contains("cp.file_hash IS NULL"), "{p}");
        assert!(
            !p.contains("cache_available"),
            "an upstream copy is not our cache: {p}"
        );
        assert!(
            !p.contains("derivation_dependency"),
            "no walk over the build graph: {p}"
        );
    }

    #[test]
    fn a_shared_build_with_no_outputs_is_not_fetchable() {
        let p = norm(&fetchable_predicate("db"));
        assert!(
            p.contains(
                "EXISTS (SELECT 1 FROM derivation_output o2 WHERE o2.derivation = db.derivation) AND NOT EXISTS"
            ),
            "the output guard must precede the NOT EXISTS: {p}"
        );
    }

    #[test]
    fn both_gate_arms_are_need_gated_and_read_the_column() {
        let sql = norm(&gates_predicate("db"));
        assert!(sql.contains("db.wanted"), "{sql}");
        assert!(
            !sql.contains("FROM derivation_dependency"),
            "the gate must not walk edges per row: {sql}"
        );
        assert!(
            sql.contains("db.blocking_deps = 0"),
            "the build arm keeps its can-start terms: {sql}"
        );
        assert!(
            sql.contains("FROM build_job bj WHERE bj.derivation = db.derivation"),
            "an unnamed shared build is still not promotable: {sql}"
        );
    }

    #[test]
    fn skipped_is_neither_a_builder_nor_pending_nor_terminal() {
        assert!(NEED_BUILD_STATUSES.contains(&BuildStatus::Skipped));
        assert!(!BUILDER_STATUSES.contains(&BuildStatus::Skipped));
        assert!(!BuildStatus::PENDING.contains(&BuildStatus::Skipped));
        assert!(!BuildStatus::TERMINAL_SUCCESS.contains(&BuildStatus::Skipped));
        assert!(!BuildStatus::FAILURE.contains(&BuildStatus::Skipped));
        assert!(!BuildStatus::REQUEUEABLE.contains(&BuildStatus::Skipped));
    }

    #[test]
    fn the_wanted_status_set_is_closed_under_both_promotion_moves() {
        for status in [BuildStatus::Created, BuildStatus::Queued] {
            assert!(
                BUILDER_STATUSES.contains(&status),
                "{status:?} is an endpoint of a promotion move"
            );
        }
    }

    #[test]
    fn gates_split_on_cache_available() {
        let g = norm(&gates_predicate("db"));
        assert!(
            g.contains(
                "AND db.wanted AND (db.cache_available OR (db.probed AND db.blocking_deps = 0 AND EXISTS ("
            ),
            "need is common to both arms, the split is on cache_available alone: {g}"
        );
        assert!(
            g.contains("JOIN cached_path cp ON cp.hash = d.hash WHERE d.id = db.derivation"),
            "the build arm reads the shared build's own .drv: {g}"
        );
    }

    #[test]
    fn promotable_is_created_plus_the_gates() {
        let p = norm(&promotable_predicate("db"));
        assert!(p.starts_with("(db.status = 0 AND ("), "{p}");
        assert!(p.contains("w.walked"), "{p}");
        assert!(p.contains("db.blocking_deps = 0"), "{p}");
        assert!(
            p.contains("FROM build_job bj WHERE bj.derivation = db.derivation"),
            "{p}"
        );
        assert!(
            !p.contains("derivation_input_source"),
            "sources are references of the .drv: {p}"
        );
    }

    #[test]
    fn the_gates_never_read_the_shared_builds_own_status_column() {
        let gates = gates_predicate("db");
        assert!(!gates.contains("db.status"), "{gates}");
        assert!(
            promotable_predicate("db").contains("db.status = 0 AND"),
            "the status term belongs to promotable alone"
        );
    }

    #[test]
    fn open_is_not_fetchable_and_not_a_terminal_failure() {
        assert_eq!(
            norm(&open_predicate("db")),
            format!(
                "(NOT db.fetchable AND db.status NOT IN ({}))",
                crate::sql::status::build_in(&BuildStatus::TERMINAL_FAILURE)
            )
        );
        for status in NEED_BUILD_STATUSES {
            assert!(
                !BuildStatus::TERMINAL_FAILURE.contains(&status),
                "{status:?} is open while it is not fetchable"
            );
        }
        for status in BuildStatus::TERMINAL_SUCCESS {
            assert!(
                !BuildStatus::TERMINAL_FAILURE.contains(&status),
                "{status:?} is open while it is not fetchable: its closure misses a dependency"
            );
        }
    }

    #[test]
    fn an_aborted_shared_build_is_open_and_thawed_by_need() {
        assert!(NEED_BUILD_STATUSES.contains(&BuildStatus::Aborted));
        assert!(BuildStatus::REQUEUEABLE.contains(&BuildStatus::Aborted));
        assert!(!BuildStatus::TERMINAL_FAILURE.contains(&BuildStatus::Aborted));
        assert!(blocks_evaluation(BuildStatus::Aborted, true));
        assert!(!blocks_evaluation(BuildStatus::Aborted, false));
    }

    #[test]
    fn the_walk_reaches_open_shared_builds_and_steps_every_edge_out_of_a_builder() {
        let cte = norm(&open_closure_cte(
            "pending",
            "SELECT $1::uuid, $2::uuid, true",
        ));
        assert!(
            cte.starts_with(
                "WITH RECURSIVE pending(evaluation, derivation, builder) AS \
                 (SELECT $1::uuid, $2::uuid, true UNION \
                 SELECT c.evaluation, s.next, s.builder FROM pending c, LATERAL ("
            ),
            "{cte}"
        );
        assert!(
            cte.contains(&format!(
                "SELECT e.dependency AS next, ({}) AS builder",
                norm(&builder_predicate("dep", "w"))
            )),
            "{cte}"
        );
        assert!(
            cte.contains(&format!(
                "WHERE e.derivation = c.derivation AND (c.builder OR e.kind IN (1, 2)) \
                 AND {} OFFSET 0) s",
                norm(&open_predicate("dep"))
            )),
            "{cte}"
        );
        assert_eq!(cte.matches("LATERAL (").count(), 1, "one probe: {cte}");
    }
}
