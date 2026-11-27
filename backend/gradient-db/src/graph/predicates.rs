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

    #[tokio::test]
    async fn shared_build_is_fetchable_only_when_it_has_stored_output_rows() {
        let [without_outputs, with_output] =
            ["1", "2"].map(|c| format!("/nix/store/{}-fetchable.drv", c.repeat(32)));
        let hash = "3".repeat(32);
        let db = gradient_fixtures::shape::shape()
            .evaluation(gradient_entity::ids::EvaluationId::now_v7())
            .derivation(&without_outputs)
            .build_status(BuildStatus::Completed)
            .derivation(&with_output)
            .output(&hash)
            .build_status(BuildStatus::Completed)
            .cached_path(&hash, "stored")
            .into_database()
            .await;
        db.execute("UPDATE derivation_build SET missing_runtime_deps = 0")
            .await;

        let fetchable = db
            .texts(&format!(
                "SELECT db.derivation::text FROM derivation_build db WHERE {}",
                fetchable_predicate("db")
            ))
            .await;
        let expected = db.id_of(&with_output).await.into_inner().to_string();
        db.finish().await;

        assert_eq!(fetchable, [expected]);
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
            assert!(BUILDER_STATUSES.contains(&status), "{status:?}");
        }
    }

    #[test]
    fn open_is_not_fetchable_and_not_a_terminal_failure() {
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

    async fn derivation_ids_matching(
        db: &gradient_fixtures::database::TestDatabase,
        predicate: &str,
    ) -> std::collections::BTreeSet<String> {
        db.texts(&format!(
            "SELECT db.derivation::text FROM derivation_build db WHERE {predicate}"
        ))
        .await
        .into_iter()
        .collect()
    }

    async fn ids_of(
        db: &gradient_fixtures::database::TestDatabase,
        paths: &[&String],
    ) -> std::collections::BTreeSet<String> {
        let mut ids = std::collections::BTreeSet::new();
        for path in paths {
            ids.insert(db.id_of(path).await.into_inner().to_string());
        }

        ids
    }

    #[tokio::test]
    async fn test_real_database_blocks_evaluation_and_open_sql_predicates_match_rust_rules_for_all_build_statuses_and_flags()
     {
        use sea_orm::Iterable;

        let db = gradient_fixtures::database::TestDatabase::create().await;
        let grid = |predicate: String| {
            format!(
                "SELECT s::text || ':' || w::text FROM generate_series(0, 10) s \
                 CROSS JOIN (VALUES (true), (false)) v(w) \
                 CROSS JOIN LATERAL (SELECT s AS status, w AS wanted, w AS fetchable) db \
                 WHERE {predicate}"
            )
        };
        let blocking: std::collections::BTreeSet<String> = db
            .texts(&grid(blocks_evaluation_predicate("db")))
            .await
            .into_iter()
            .collect();
        let open: std::collections::BTreeSet<String> = db
            .texts(&grid(open_predicate("db")))
            .await
            .into_iter()
            .collect();
        db.finish().await;

        let mut expected_blocking = std::collections::BTreeSet::new();
        let mut expected_open = std::collections::BTreeSet::new();
        for status in BuildStatus::iter() {
            for flag in [true, false] {
                let key = format!("{}:{flag}", i32::from(status));
                if blocks_evaluation(status, flag) {
                    expected_blocking.insert(key.clone());
                }
                if !flag && !BuildStatus::TERMINAL_FAILURE.contains(&status) {
                    expected_open.insert(key);
                }
            }
        }
        assert_eq!(blocking, expected_blocking);
        assert_eq!(open, expected_open);
    }

    #[tokio::test]
    async fn gates_and_promotable_predicates_accept_build_only_when_walked_named_wanted_and_cache_available_or_probe_has_no_blocking_deps_and_stored_drv()
     {
        let path = |name: &str| format!("/nix/store/{}-gate.drv", name.repeat(32));
        let ready_shape = |shape: gradient_fixtures::shape::Shape, key: &str| {
            shape.derivation(&path(key)).walked().job().drv_cached()
        };
        let shape = gradient_fixtures::shape::shape()
            .evaluation(gradient_entity::ids::EvaluationId::now_v7());
        let shape = ["1", "4", "5", "6", "9"]
            .into_iter()
            .fold(shape, ready_shape);
        let db = shape
            .derivation(&path("2"))
            .job()
            .drv_cached()
            .derivation(&path("3"))
            .walked()
            .drv_cached()
            .derivation(&path("7"))
            .walked()
            .job()
            .derivation(&path("8"))
            .walked()
            .job()
            .into_database()
            .await;
        db.execute("UPDATE derivation_build SET wanted = true, probed = true, blocking_deps = 0")
            .await;
        db.execute(&format!(
            "UPDATE derivation_build SET wanted = false WHERE derivation = '{}'; \
             UPDATE derivation_build SET probed = false WHERE derivation = '{}'; \
             UPDATE derivation_build SET blocking_deps = 1 WHERE derivation = '{}'; \
             UPDATE derivation_build SET status = 1 WHERE derivation = '{}'",
            db.id_of(&path("4")).await,
            db.id_of(&path("5")).await,
            db.id_of(&path("6")).await,
            db.id_of(&path("9")).await,
        ))
        .await;
        db.execute(&format!(
            "UPDATE derivation_build SET cache_available = true, probed = false, blocking_deps = 5 \
             WHERE derivation = '{}'",
            db.id_of(&path("8")).await,
        ))
        .await;

        let gated = derivation_ids_matching(&db, &gates_predicate("db")).await;
        let promotable = derivation_ids_matching(&db, &promotable_predicate("db")).await;
        let (ready, cached, queued) = (path("1"), path("8"), path("9"));
        let expected_gated = ids_of(&db, &[&ready, &cached, &queued]).await;
        let expected_promotable = ids_of(&db, &[&ready, &cached]).await;
        db.finish().await;

        assert_eq!(gated, expected_gated);
        assert_eq!(promotable, expected_promotable);
    }

    #[tokio::test]
    async fn test_fetchable_predicate_and_settled_incomplete_predicate_on_database_builds_with_different_status_missing_runtime_dependencies_unneeded_outputs_cache_availability_and_stored_outputs()
     {
        let path = |key: &str| format!("/nix/store/{}-fetch.drv", key.repeat(32));
        let stored = |shape: gradient_fixtures::shape::Shape, key: &str, status: BuildStatus| {
            shape
                .derivation(&path(key))
                .output(&key.repeat(32))
                .build_status(status)
                .cached_path(&key.repeat(32), key)
        };
        let shape = gradient_fixtures::shape::shape()
            .evaluation(gradient_entity::ids::EvaluationId::now_v7());
        let shape = [
            ("1", BuildStatus::Completed),
            ("2", BuildStatus::Substituted),
            ("3", BuildStatus::Queued),
            ("4", BuildStatus::Completed),
            ("5", BuildStatus::Queued),
        ]
        .into_iter()
        .fold(shape, |shape, (key, status)| stored(shape, key, status));
        let unneeded_hash = format!("{}d", "6".repeat(31));
        let db = shape
            .derivation(&path("6"))
            .output(&"6".repeat(32))
            .unneeded_output(&unneeded_hash, "dev")
            .build_status(BuildStatus::Completed)
            .cached_path(&"6".repeat(32), "6")
            .derivation(&path("7"))
            .output(&"7".repeat(32))
            .build_status(BuildStatus::Completed)
            .derivation(&path("8"))
            .build_status(BuildStatus::Completed)
            .derivation(&path("9"))
            .output(&"9".repeat(32))
            .cache_available()
            .build_status(BuildStatus::Completed)
            .into_database()
            .await;
        db.execute(&format!(
            "UPDATE derivation_build SET missing_runtime_deps = 0; \
             UPDATE derivation_build SET missing_runtime_deps = 1 WHERE derivation IN ('{}', '{}')",
            db.id_of(&path("4")).await,
            db.id_of(&path("5")).await,
        ))
        .await;

        let fetchable = derivation_ids_matching(&db, &fetchable_predicate("db")).await;
        let incomplete = derivation_ids_matching(&db, &settled_incomplete_predicate("db")).await;
        let (completed, substituted, unneeded_unstored) = (path("1"), path("2"), path("6"));
        let expected_fetchable = ids_of(&db, &[&completed, &substituted, &unneeded_unstored]).await;
        let settled_missing_runtime_dependency = path("4");
        let expected_incomplete = ids_of(&db, &[&settled_missing_runtime_dependency]).await;
        db.finish().await;

        assert_eq!(fetchable, expected_fetchable);
        assert_eq!(incomplete, expected_incomplete);
    }
}
