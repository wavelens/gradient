/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::EvaluationStatus;

pub fn drv_present_predicate(alias: &str) -> String {
    format!(
        "EXISTS (SELECT 1 FROM derivation d JOIN cached_path cp ON cp.hash = d.hash \
         WHERE d.id = {alias}.derivation)"
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

pub const NEED_BUILD_STATUSES: [BuildStatus; 5] = [
    BuildStatus::Created,
    BuildStatus::Queued,
    BuildStatus::Building,
    BuildStatus::FailedTransient,
    BuildStatus::Skipped,
];

pub const BUILDS_ONCE_WANTED: [BuildStatus; 6] = [
    BuildStatus::Created,
    BuildStatus::Queued,
    BuildStatus::Building,
    BuildStatus::Aborted,
    BuildStatus::FailedTransient,
    BuildStatus::Skipped,
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

pub fn aborted_in_evaluation(evaluation: &str) -> String {
    format!(
        "SELECT bj.derivation FROM build_job bj JOIN derivation_build ab ON ab.id = bj.derivation_build \
         WHERE bj.evaluation = {evaluation} AND bj.aborted AND ab.status = {aborted}",
        aborted = crate::sql::status::build(BuildStatus::Aborted),
    )
}

pub fn builder_predicate(shared_build: &str, walked: &str) -> String {
    builds_here(shared_build, walked, &BUILDER_STATUSES)
}

pub fn builds_once_wanted_predicate(shared_build: &str, walked: &str) -> String {
    builds_here(shared_build, walked, &BUILDS_ONCE_WANTED)
}

fn builds_here(shared_build: &str, walked: &str, statuses: &[BuildStatus]) -> String {
    format!(
        "{walked}.walked AND {shared_build}.probed AND NOT {shared_build}.cache_available \
         AND {shared_build}.status IN ({pending})",
        pending = crate::sql::status::build_in(statuses),
    )
}

pub fn open_predicate(alias: &str) -> String {
    format!(
        "(NOT {alias}.fetchable AND {alias}.status NOT IN ({failed}))",
        failed = crate::sql::status::build_in(&BuildStatus::TERMINAL_FAILURE),
    )
}

/// Parents built here need every dependency. Parents available in a cache only need the
/// dependency outputs their own outputs reference and the reference probe joins under the
/// passthrough row for the planner to price it by the passthrough share.
pub fn failure_spreads_predicate(edge: &str) -> String {
    format!(
        "(NOT EXISTS (SELECT 1 FROM derivation_build rb \
         WHERE rb.derivation = {edge}.derivation AND rb.cache_available \
           AND NOT {referenced_unavailable}) \
         AND NOT EXISTS (SELECT 1 FROM derivation_build fb \
         WHERE fb.derivation = {edge}.dependency AND fb.fetchable))",
        referenced_unavailable = references_unavailable_output(edge, "rb.derivation"),
    )
}

pub fn aborted_dependency_without_failure_spread_predicate(
    edge: &str,
    dependency_build_alias: &str,
    parent_derivation_build_alias: Option<&str>,
) -> String {
    let parent_cache_available_and_all_referenced_dependency_outputs_are_held_somewhere =
        match parent_derivation_build_alias {
            Some(parent) => format!(
                "({parent}.cache_available AND NOT {referenced_unavailable})",
                referenced_unavailable =
                    references_unavailable_output(edge, &format!("{parent}.derivation")),
            ),
            None => format!(
                "EXISTS (SELECT 1 FROM derivation_build rb \
                 WHERE rb.derivation = {edge}.derivation AND rb.cache_available \
                   AND NOT {referenced_unavailable})",
                referenced_unavailable = references_unavailable_output(edge, "rb.derivation"),
            ),
        };
    format!(
        "({dependency_build_alias}.status = {aborted} \
         AND ({dependency_build_alias}.fetchable \
              OR {parent_cache_available_and_all_referenced_dependency_outputs_are_held_somewhere}))",
        aborted = crate::sql::status::build(BuildStatus::Aborted),
    )
}

fn references_unavailable_output(edge: &str, parent: &str) -> String {
    format!(
        "EXISTS (SELECT 1 FROM derivation_output o \
         WHERE o.derivation = {edge}.dependency AND {unavailable} \
           AND EXISTS (SELECT 1 FROM derivation_output po \
                       LEFT JOIN cached_path pcp ON pcp.hash = po.hash \
                       WHERE po.derivation = {parent} \
                         AND CASE WHEN {refs} IS NULL THEN {edge}.kind IN (1, 2) \
                                  ELSE string_to_array({refs}, ' ') \
                                       && {tokens} END))",
        unavailable = sql_condition_for_derivation_output_without_cached_path_and_external_url("o"),
        refs = PARENT_REFERENCES,
        tokens = store_path_tokens("o"),
    )
}

pub fn sql_condition_for_derivation_output_without_cached_path_and_external_url(
    output: &str,
) -> String {
    format!(
        "({output}.external_url IS NULL AND {without_cached_path})",
        without_cached_path = sql_condition_for_derivation_output_without_cached_path(output),
    )
}

pub fn sql_condition_for_derivation_output_without_cached_path(output: &str) -> String {
    format!("NOT EXISTS (SELECT 1 FROM cached_path cp WHERE cp.hash = {output}.hash)")
}

const PARENT_REFERENCES: &str = "coalesce(pcp.\"references\", po.references_list)";

fn store_path_tokens(output: &str) -> String {
    format!(
        "ARRAY[{output}.hash || '-' || {output}.package, \
               '/nix/store/' || {output}.hash || '-' || {output}.package]"
    )
}

pub fn output_needed_predicate(output: &str) -> String {
    format!(
        "({direct} OR EXISTS (WITH RECURSIVE referrer(id, hash, package) AS ( \
             SELECT s.id, s.hash, s.package FROM derivation_output s \
             LEFT JOIN cached_path scp ON scp.hash = s.hash \
             WHERE s.derivation = {output}.derivation AND s.id <> {output}.id \
               AND string_to_array({sibling_refs}, ' ') && {tokens} \
             UNION \
             SELECT s.id, s.hash, s.package FROM referrer referenced \
             JOIN derivation_output s ON s.derivation = {output}.derivation AND s.id <> referenced.id \
             LEFT JOIN cached_path scp ON scp.hash = s.hash \
             WHERE string_to_array({sibling_refs}, ' ') && {referenced_tokens}) \
           SELECT 1 FROM referrer JOIN derivation_output s ON s.id = referrer.id \
           WHERE {sibling_direct}))",
        direct = directly_needed_predicate(output),
        sibling_refs = SIBLING_REFERENCES,
        tokens = store_path_tokens(output),
        referenced_tokens = store_path_tokens("referenced"),
        sibling_direct = directly_needed_predicate("s"),
    )
}

const SIBLING_REFERENCES: &str = "coalesce(scp.\"references\", s.references_list)";

fn directly_needed_predicate(output: &str) -> String {
    format!(
        "(EXISTS (SELECT 1 FROM entry_point ep {live} WHERE ep.derivation = {output}.derivation) \
         OR EXISTS (SELECT 1 FROM derivation_dependency e \
                    CROSS JOIN LATERAL (SELECT 1 FROM derivation_build p \
                                        JOIN derivation pw ON pw.id = p.derivation \
                                        WHERE p.derivation = e.derivation \
                                          AND p.wanted AND {open} AND ({builder}) \
                                        OFFSET 0) wanted_open_parent_build \
                    WHERE e.dependency = {output}.derivation AND e.kind IN (0, 2) \
                      AND (e.dependency_outputs IS NULL \
                           OR {output}.name = ANY(e.dependency_outputs))) \
         OR EXISTS (SELECT 1 FROM derivation_dependency e \
                    CROSS JOIN LATERAL (SELECT 1 FROM derivation_build p \
                                        WHERE p.derivation = e.derivation AND p.wanted AND {open} \
                                        OFFSET 0) wanted_open_parent, \
                    LATERAL (SELECT {refs} AS refs FROM derivation_output po \
                             LEFT JOIN cached_path pcp ON pcp.hash = po.hash \
                             WHERE po.derivation = e.derivation \
                               AND (pcp.hash IS NOT NULL OR po.references_list IS NOT NULL) \
                             OFFSET 0) r \
                    WHERE e.dependency = {output}.derivation AND e.kind IN (1, 2) \
                      AND (r.refs IS NULL OR string_to_array(r.refs, ' ') && {tokens})))",
        live = crate::graph::can_start::wanted_by_its_evaluation("ep"),
        open = open_predicate("p"),
        builder = builds_once_wanted_predicate("p", "pw"),
        refs = PARENT_REFERENCES,
        tokens = store_path_tokens(output),
    )
}

pub fn builds_with_unneeded_output_predicate(edge: &str, dependency: &str) -> String {
    format!(
        "({dependency}.unneeded_outputs <> '{{}}' AND ({edge}.dependency_outputs IS NULL \
         OR {edge}.dependency_outputs && {dependency}.unneeded_outputs))"
    )
}

pub fn settled_incomplete_predicate(alias: &str) -> String {
    format!(
        "({alias}.status IN ({terminal_success}) AND {alias}.missing_runtime_deps > 0)",
        terminal_success = crate::sql::status::build_in(&BuildStatus::TERMINAL_SUCCESS),
    )
}

/// The `EXISTS` over `derivation_output` is load-bearing.
/// The `NOT EXISTS` is vacuously true for a shared build with no output rows.
/// Parents without the guard start against an input nothing can provide.
pub fn fetchable_predicate(alias: &str) -> String {
    format!(
        "({alias}.status IN ({terminal_success}) AND {complete})",
        terminal_success = crate::sql::status::build_in(&BuildStatus::TERMINAL_SUCCESS),
        complete = shared_build_complete_predicate(alias),
    )
}

/// The presence predicate must count every content variant of each output hash. A check in `store_path_content_variant_reader_choice` at read time tells whether a reader can consume that variant.
pub fn present_predicate(alias: &str) -> String {
    format!(
        "(EXISTS (SELECT 1 FROM derivation_output o2 WHERE o2.derivation = {alias}.derivation) \
         AND NOT EXISTS (SELECT 1 FROM derivation_output o \
                         WHERE o.derivation = {alias}.derivation AND {needed_without_cached_path}))",
        needed_without_cached_path =
            sql_condition_for_needed_output_without_cached_path("o", alias),
    )
}

pub fn sql_condition_for_needed_output_without_cached_path(output: &str, build: &str) -> String {
    format!(
        "{output}.name <> ALL({build}.unneeded_outputs) AND {without_cached_path}",
        without_cached_path = sql_condition_for_derivation_output_without_cached_path(output),
    )
}

fn all_outputs_cached_or_unneeded(alias: &str) -> String {
    format!("bool_and(cp.id IS NOT NULL OR o.name = ANY({alias}.unneeded_outputs))")
}

pub fn present_value(alias: &str) -> String {
    format!(
        "coalesce((SELECT {all_outputs} \
                   FROM derivation_output o LEFT JOIN cached_path cp ON cp.hash = o.hash \
                   WHERE o.derivation = {alias}.derivation), false)",
        all_outputs = all_outputs_cached_or_unneeded(alias),
    )
}

pub fn present_shared_build_derivation_ids_select() -> String {
    format!(
        "SELECT db.derivation FROM derivation_output o \
         JOIN derivation_build db ON db.derivation = o.derivation \
         LEFT JOIN cached_path cp ON cp.hash = o.hash \
         GROUP BY db.derivation \
         HAVING {all_outputs}",
        all_outputs = all_outputs_cached_or_unneeded("db"),
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
        created = crate::sql::status::from(BuildStatus::QUEUE),
        gates = gates_predicate(alias),
    )
}

pub(crate) fn not_aborted_build_jobs_with_unfinished_evaluation_sql_for_derivation_build(
    shared_build: &str,
) -> String {
    format!(
        "FROM build_job live_job \
         JOIN evaluation live_evaluation ON live_evaluation.id = live_job.evaluation \
         JOIN task live_task ON live_task.id = live_evaluation.task \
         WHERE live_job.derivation = {shared_build}.derivation \
           AND live_job.derivation_build = {shared_build}.id \
           AND live_job.evaluation = ANY (ARRAY(SELECT unfinished_evaluation.id \
               FROM evaluation unfinished_evaluation \
               WHERE unfinished_evaluation.status NOT IN ({terminal}))) \
           AND NOT live_job.aborted",
        terminal = crate::sql::status::eval_in(&EvaluationStatus::TERMINAL),
    )
}

pub const PUBLISH_JOB_MAX_AGE_SECONDS: u64 = 24 * 60 * 60;

pub(crate) fn not_aborted_build_jobs_with_unfinished_or_recently_finished_evaluation_sql_for_derivation_build(
    shared_build: &str,
) -> String {
    format!(
        "FROM build_job live_job \
         JOIN evaluation live_evaluation ON live_evaluation.id = live_job.evaluation \
         JOIN task live_task ON live_task.id = live_evaluation.task \
         WHERE live_job.derivation_build = {shared_build}.id AND NOT live_job.aborted \
           AND (live_evaluation.status NOT IN ({terminal}) \
                OR live_evaluation.finished_at >= (now() AT TIME ZONE 'UTC') - interval '{PUBLISH_JOB_MAX_AGE_SECONDS} seconds')",
        terminal = crate::sql::status::eval_in(&EvaluationStatus::TERMINAL),
    )
}

pub fn derivation_build_has_build_job_not_aborted_and_evaluation_not_finished_sql_predicate(
    shared_build: &str,
) -> String {
    format!(
        "EXISTS (SELECT 1 {live_jobs})",
        live_jobs = not_aborted_build_jobs_with_unfinished_evaluation_sql_for_derivation_build(
            shared_build
        )
    )
}

pub fn content_variant_trusted_by_all_projects_with_live_job_on_shared_build(
    variant: &str,
    shared_build: &str,
) -> String {
    format!(
        "NOT EXISTS (SELECT 1 {live_jobs} AND NOT {trusted})",
        live_jobs = not_aborted_build_jobs_with_unfinished_evaluation_sql_for_derivation_build(
            shared_build
        ),
        trusted =
            crate::store_path_content_variant_trust::project_trusts_content_variant_exists_sql(
                variant,
                "live_task.project"
            ),
    )
}

/// Jobs in running evaluations decide which project inputs a shared build can read. Completed and failed evaluations take over once no running jobs remain. They still want rebuilds of retired outputs. The scheduler can dispatch those rebuilds.
fn not_aborted_build_jobs_with_unfinished_or_finished_evaluation_sql_for_derivation_build_inputs(
    shared_build: &str,
) -> String {
    format!(
        "FROM build_job input_reader_job \
         JOIN evaluation input_reader_evaluation ON input_reader_evaluation.id = input_reader_job.evaluation \
         JOIN task input_reader_task ON input_reader_task.id = input_reader_evaluation.task \
         WHERE input_reader_job.derivation_build = {shared_build}.id AND NOT input_reader_job.aborted \
           AND input_reader_evaluation.status <> {aborted} \
           AND (input_reader_evaluation.status NOT IN ({terminal}) OR NOT {live})",
        aborted = crate::sql::status::eval(EvaluationStatus::Aborted),
        terminal = crate::sql::status::eval_in(&EvaluationStatus::TERMINAL),
        live = derivation_build_has_build_job_not_aborted_and_evaluation_not_finished_sql_predicate(
            shared_build
        ),
    )
}

pub fn shared_build_has_build_job_not_aborted_and_evaluation_not_aborted_sql_predicate(
    shared_build: &str,
) -> String {
    format!(
        "EXISTS (SELECT 1 {jobs})",
        jobs =
            not_aborted_build_jobs_with_unfinished_or_finished_evaluation_sql_for_derivation_build_inputs(
                shared_build
            ),
    )
}

pub fn stored_copy_in_input_closure_or_trusted_by_every_project_with_build_job_on_shared_build_sql_condition(
    variant: &str,
    shared_build: &str,
    store_path_hash_in_input_closure_sql_condition: &str,
) -> String {
    format!(
        "({store_path_hash_in_input_closure_sql_condition} OR NOT EXISTS (SELECT 1 {jobs} AND NOT {trusted}))",
        jobs =
            not_aborted_build_jobs_with_unfinished_or_finished_evaluation_sql_for_derivation_build_inputs(
                shared_build
            ),
        trusted =
            crate::store_path_content_variant_trust::project_trusts_content_variant_exists_sql(
                variant,
                "input_reader_task.project"
            ),
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
        for status in [
            Completed,
            Substituted,
            FailedPermanent,
            DependencyFailed,
            Aborted,
        ] {
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
            g.contains("JOIN cached_path cp ON cp.hash = d.hash")
                && !g.contains("missing_references"),
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
             WHERE o.derivation = db.derivation AND o.name <> ALL(db.unneeded_outputs) \
             AND NOT EXISTS (SELECT 1 FROM cached_path cp WHERE cp.hash = o.hash))))"
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
        assert!(
            p.contains("NOT EXISTS (SELECT 1 FROM cached_path cp WHERE cp.hash = o.hash)")
                && !p.contains("file_hash"),
            "{p}"
        );
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
    fn a_settled_build_missing_a_runtime_dependency_is_incomplete() {
        assert_eq!(
            settled_incomplete_predicate("p"),
            "(p.status IN (3, 7) AND p.missing_runtime_deps > 0)"
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
    fn the_walk_steps_through_every_builder_and_everything_a_thaw_would_build() {
        for status in BUILDER_STATUSES {
            assert!(BUILDS_ONCE_WANTED.contains(&status), "{status:?}");
        }
        for status in BuildStatus::THAW_WANTED.from {
            assert!(BUILDS_ONCE_WANTED.contains(status), "{status:?}");
        }
        assert_eq!(
            BUILDS_ONCE_WANTED.len(),
            BUILDER_STATUSES.len() + BuildStatus::THAW_WANTED.from.len()
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
    fn an_aborted_shared_build_is_open_and_thawed_by_need_but_holds_no_evaluation() {
        assert!(BuildStatus::REQUEUEABLE.contains(&BuildStatus::Aborted));
        assert!(!BuildStatus::TERMINAL_FAILURE.contains(&BuildStatus::Aborted));
        assert!(!blocks_evaluation(BuildStatus::Aborted, true));
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

    const SHARED_BUILD_OUTPUT_STORE_PATH_HASH: &str = "88888888888888888888888888888888";
    const FINISHED_EVALUATION_SHARED_BUILD_OUTPUT_STORE_PATH_HASH: &str =
        "99999999999999999999999999999999";

    fn test_only_old_not_aborted_build_jobs_with_unfinished_evaluation_sql_for_derivation_build(
        shared_build: &str,
    ) -> String {
        format!(
            "FROM build_job live_job \
             JOIN evaluation live_evaluation ON live_evaluation.id = live_job.evaluation \
             JOIN task live_task ON live_task.id = live_evaluation.task \
             WHERE live_job.derivation_build = {shared_build}.id AND NOT live_job.aborted \
               AND live_evaluation.status NOT IN ({terminal})",
            terminal = crate::sql::status::eval_in(&EvaluationStatus::TERMINAL),
        )
    }

    async fn run_live_build_job_fragment_statements_and_collect_sorted_labelled_rows(
        db: &gradient_fixtures::database::TestDatabase,
        live_jobs: fn(&str) -> String,
    ) -> Vec<String> {
        use sea_orm::{ConnectionTrait, TransactionTrait, Value};

        let render = |label: &str, text: &str| {
            let aliases = ["reader_build", "db", "p"];
            assert!(
                aliases.iter().any(|alias| text.contains(
                    &not_aborted_build_jobs_with_unfinished_evaluation_sql_for_derivation_build(
                        alias
                    )
                )),
                "The fragment for the live build job is part of statement {label}."
            );
            aliases.iter().fold(text.to_owned(), |text, alias| {
                text.replace(
                    &not_aborted_build_jobs_with_unfinished_evaluation_sql_for_derivation_build(
                        alias,
                    ),
                    &live_jobs(alias),
                )
            })
        };
        let registered = |name: &str| {
            render(
                name,
                &crate::sql::query::registry()
                    .find(|query| query.name == name)
                    .unwrap_or_else(|| {
                        panic!("The entries in the statement registry include {name}.")
                    })
                    .text(),
            )
        };
        let uuids = |sql: &'static str| async move {
            db.texts(sql)
                .await
                .iter()
                .map(|id| id.parse::<uuid::Uuid>().expect("a uuid"))
                .collect::<Vec<_>>()
        };
        let outputs = uuids("SELECT derivation::text FROM derivation_output ORDER BY hash").await;
        let hashes = db
            .texts("SELECT hash FROM derivation_output ORDER BY hash")
            .await;
        let evaluations = uuids("SELECT id::text FROM evaluation ORDER BY id").await;
        let shared_builds = uuids("SELECT id::text FROM derivation_build ORDER BY id").await;
        let per_evaluation = |extra: &[Value]| {
            evaluations
                .iter()
                .map(|evaluation| {
                    let mut values = extra.to_vec();
                    values.push(Value::Uuid(Some(*evaluation)));
                    values
                })
                .collect::<Vec<_>>()
        };
        let runs: Vec<(&str, String, Vec<Vec<Value>>)> = vec![
            (
                "live jobs",
                format!(
                    "SELECT DISTINCT sb.id AS shared_build, live.* FROM derivation_build sb \
                     CROSS JOIN LATERAL (SELECT live_task.project {}) live",
                    live_jobs("sb")
                ),
                vec![vec![]],
            ),
            (
                "graph writer",
                render(
                    "graph writer",
                    &format!(
                        "SELECT output.id FROM derivation_output output \
                         JOIN derivation_build reader_build ON reader_build.derivation = output.derivation \
                         WHERE output.derivation = ANY($1::uuid[]) AND output.hash IS NOT NULL \
                           AND EXISTS ({})",
                        crate::store_path_content_variant_reader_choice::select_best_stored_copy_for_own_build_outputs_trusted_by_all_projects_with_live_jobs_excluding_direct_use(
                            "output.hash",
                            "reader_build",
                        ),
                    ),
                ),
                vec![vec![outputs.clone().into()]],
            ),
            (
                "own outputs",
                registered(
                    "DERIVATION_ID_AND_TRUSTED_CONTENT_VARIANT_ROW_FOR_OWN_OUTPUT_STORE_PATH_HASHES_ON_SHARED_BUILD_QUERY",
                ),
                vec![vec![outputs.clone().into(), hashes.clone().into()]],
            ),
            (
                "build input",
                registered("STORED_COPY_BY_STORE_PATH_HASH_USABLE_AS_SHARED_BUILD_BUILD_INPUT_QUERY"),
                shared_builds
                    .iter()
                    .map(|shared_build| {
                        vec![
                            hashes.clone().into(),
                            Value::Uuid(Some(*shared_build)),
                            Vec::<String>::new().into(),
                        ]
                    })
                    .collect(),
            ),
            (
                "complete output hashes",
                registered("COMPLETE_OUTPUT_HASHES"),
                per_evaluation(&[hashes.clone().into()]),
            ),
            (
                "repair cached shared builds",
                registered("REPAIR_CACHED_SHARED_BUILDS_FOR_EVAL"),
                per_evaluation(&[]),
            ),
            (
                "needed outputs without stored copy",
                registered(
                    "STORE_PATH_HASHES_OF_NEEDED_OUTPUTS_WITHOUT_STORED_COPY_OLDEST_FIRST_LIMIT",
                ),
                vec![vec![64_i64.into()]],
            ),
        ];
        let connection = db.connection();
        let mut rows = Vec::new();
        for (label, sql, binds) in runs {
            for (bind, values) in binds.into_iter().enumerate() {
                let transaction = connection.begin().await.expect("a transaction");
                let result = transaction
                    .query_all_raw(
                        crate::sql::query::registry()
                            .find(|query| query.name == "COMPLETE_OUTPUT_HASHES")
                            .expect(
                                "The contents of the statement registry include `COMPLETE_OUTPUT_HASHES`.",
                            )
                            .bind_built(
                                format!(
                                    "WITH statement_result_rows_as_json_text_lines AS ({sql}) \
                                     SELECT to_jsonb(statement_result_rows_as_json_text_lines)::text \
                                     FROM statement_result_rows_as_json_text_lines ORDER BY 1"
                                ),
                                values,
                            ),
                    )
                    .await
                    .unwrap_or_else(|error| panic!("Statement {label} executed. {error}"));
                transaction.rollback().await.expect("a rollback");
                rows.extend(result.iter().map(|row| {
                    format!(
                        "{label} {bind} {}",
                        row.try_get_by_index::<String>(0).expect("a text column")
                    )
                }));
            }
        }
        rows.sort();
        rows
    }

    async fn assert_old_and_new_rows_equal_and_graph_writer_returns_output_id_for_store_path_hash(
        db: &gradient_fixtures::database::TestDatabase,
        hash: &str,
    ) -> bool {
        let old = run_live_build_job_fragment_statements_and_collect_sorted_labelled_rows(
            db,
            test_only_old_not_aborted_build_jobs_with_unfinished_evaluation_sql_for_derivation_build,
        )
        .await;
        let new = run_live_build_job_fragment_statements_and_collect_sorted_labelled_rows(
            db,
            not_aborted_build_jobs_with_unfinished_evaluation_sql_for_derivation_build,
        )
        .await;
        assert!(
            !old.is_empty(),
            "The old fragment can return at least one row in this state."
        );
        assert_eq!(
            old, new,
            "The old live build job fragment and the new live build job fragment return the same rows for every statement."
        );
        let output = db
            .texts(&format!(
                "SELECT id::text FROM derivation_output WHERE hash = '{hash}'"
            ))
            .await;
        new.iter()
            .any(|row| row.starts_with("graph writer") && row.contains(&output[0]))
    }

    #[tokio::test]
    async fn test_live_build_job_sql_fragments_return_same_rows_on_fixture_states() {
        use gradient_types::EvaluationId;

        let unfinished_project_a_evaluation_id_on_shared_build = EvaluationId::now_v7();
        let completed_evaluation_id_of_project_b_with_shared_build_job = EvaluationId::now_v7();
        let second_unfinished_evaluation_id = EvaluationId::now_v7();
        let project_d_failed_evaluation_id_with_no_unfinished_evaluation_build_jobs =
            EvaluationId::now_v7();
        let shared = format!("/nix/store/{}-shared.drv", "s".repeat(32));
        let db = gradient_fixtures::shape::shape()
            .evaluation(unfinished_project_a_evaluation_id_on_shared_build)
            .derivation(&shared)
            .output(SHARED_BUILD_OUTPUT_STORE_PATH_HASH)
            .job()
            .cached_path(SHARED_BUILD_OUTPUT_STORE_PATH_HASH, "shared")
            .evaluation(completed_evaluation_id_of_project_b_with_shared_build_job)
            .evaluation_status(EvaluationStatus::Completed)
            .derivation(&shared)
            .job()
            .evaluation(second_unfinished_evaluation_id)
            .derivation(&shared)
            .job()
            .evaluation(project_d_failed_evaluation_id_with_no_unfinished_evaluation_build_jobs)
            .evaluation_status(EvaluationStatus::Failed)
            .derivation(&format!("/nix/store/{}-finished.drv", "f".repeat(32)))
            .output(FINISHED_EVALUATION_SHARED_BUILD_OUTPUT_STORE_PATH_HASH)
            .job()
            .cached_path(
                FINISHED_EVALUATION_SHARED_BUILD_OUTPUT_STORE_PATH_HASH,
                "finished",
            )
            .into_database()
            .await;
        let insert_trusted_stored_copy_producer_row = |evaluation: EvaluationId, hash: &str| {
            format!(
                "INSERT INTO store_path_content_producer (cached_path, kind, uploading_member_project_id) \
                 SELECT cp.id, 2, t.project FROM cached_path cp, evaluation e JOIN task t ON t.id = e.task \
                 WHERE cp.hash = '{hash}' AND e.id = '{evaluation}';"
            )
        };
        db.execute(&format!(
            "DELETE FROM store_path_content_producer; {} {} \
             UPDATE build_job SET aborted = true WHERE evaluation = '{second_unfinished_evaluation_id}'; {}",
            insert_trusted_stored_copy_producer_row(
                unfinished_project_a_evaluation_id_on_shared_build,
                SHARED_BUILD_OUTPUT_STORE_PATH_HASH
            ),
            insert_trusted_stored_copy_producer_row(
                project_d_failed_evaluation_id_with_no_unfinished_evaluation_build_jobs,
                FINISHED_EVALUATION_SHARED_BUILD_OUTPUT_STORE_PATH_HASH
            ),
            insert_trusted_stored_copy_producer_row(
                second_unfinished_evaluation_id,
                SHARED_BUILD_OUTPUT_STORE_PATH_HASH
            ),
        ))
        .await;
        assert!(
            assert_old_and_new_rows_equal_and_graph_writer_returns_output_id_for_store_path_hash(
                &db,
                SHARED_BUILD_OUTPUT_STORE_PATH_HASH
            )
            .await,
            "Build jobs of completed evaluations don't count, so the graph writer can return the trusted stored copy of the shared build output."
        );

        db.execute(&format!(
            "DELETE FROM store_path_content_producer USING evaluation e JOIN task t ON t.id = e.task \
             WHERE e.id = '{second_unfinished_evaluation_id}' \
               AND store_path_content_producer.uploading_member_project_id = t.project; {}",
            insert_trusted_stored_copy_producer_row(
                completed_evaluation_id_of_project_b_with_shared_build_job,
                SHARED_BUILD_OUTPUT_STORE_PATH_HASH
            ),
        ))
        .await;
        assert!(
            assert_old_and_new_rows_equal_and_graph_writer_returns_output_id_for_store_path_hash(
                &db,
                SHARED_BUILD_OUTPUT_STORE_PATH_HASH
            )
            .await,
            "Aborted build jobs don't count. The graph writer can return a trusted saved copy of the shared build output."
        );

        db.execute(&format!(
            "UPDATE build_job SET aborted = false WHERE evaluation = '{second_unfinished_evaluation_id}'"
        ))
        .await;
        assert!(
            !assert_old_and_new_rows_equal_and_graph_writer_returns_output_id_for_store_path_hash(
                &db,
                SHARED_BUILD_OUTPUT_STORE_PATH_HASH
            )
            .await,
            "Projects with live build jobs that don't trust stored copies hide shared build outputs from the graph writer."
        );

        db.execute(&insert_trusted_stored_copy_producer_row(
            second_unfinished_evaluation_id,
            SHARED_BUILD_OUTPUT_STORE_PATH_HASH,
        ))
        .await;
        assert!(
            assert_old_and_new_rows_equal_and_graph_writer_returns_output_id_for_store_path_hash(
                &db,
                SHARED_BUILD_OUTPUT_STORE_PATH_HASH
            )
            .await,
            "The graph writer can return the shared build output if both projects have running build jobs and trust the stored copy."
        );
        assert!(
            assert_old_and_new_rows_equal_and_graph_writer_returns_output_id_for_store_path_hash(
                &db,
                FINISHED_EVALUATION_SHARED_BUILD_OUTPUT_STORE_PATH_HASH
            )
            .await,
            "A shared build with no live build job returns its stored output to the graph writer."
        );

        db.execute(&format!(
            "INSERT INTO build_job (id, evaluation, derivation, derivation_build, created_at) \
             SELECT gen_random_uuid(), '{unfinished_project_a_evaluation_id_on_shared_build}', \
                    finished.derivation, shared_build.id, now() \
             FROM derivation_output finished, derivation_build shared_build \
             JOIN derivation_output shared ON shared.derivation = shared_build.derivation \
             WHERE finished.hash = '{FINISHED_EVALUATION_SHARED_BUILD_OUTPUT_STORE_PATH_HASH}' \
               AND shared.hash = '{SHARED_BUILD_OUTPUT_STORE_PATH_HASH}'"
        ))
        .await;
        assert!(
            assert_old_and_new_rows_equal_and_graph_writer_returns_output_id_for_store_path_hash(
                &db,
                FINISHED_EVALUATION_SHARED_BUILD_OUTPUT_STORE_PATH_HASH
            )
            .await,
            "A build job whose shared build belongs to another derivation does not count as a live build job for either derivation."
        );
        db.finish().await;
    }
}
