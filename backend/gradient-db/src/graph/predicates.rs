/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::build::BuildStatus;

/// The build target `{alias}`'s own `.drv` NAR is in our cache. Its closure is
/// trusted: the evaluation pushed it before it reported the derivation, so this is
/// the "the worker can fetch and import the input-`.drv` closure" signal. The exact
/// negation of [`drv_nar_absent_predicate`], which is what condemns an evaluation.
pub fn drv_present_predicate(alias: &str) -> String {
    format!(
        "EXISTS (SELECT 1 FROM derivation d JOIN cached_path cp ON cp.hash = d.hash \
         WHERE d.id = {alias}.derivation AND cp.file_hash IS NOT NULL)"
    )
}

/// The build target `{alias}`'s own `.drv` NAR is not in our cache at all: no
/// `cached_path` row, or a row with no backing NAR. This is the only `.drv`
/// state a fresh evaluation repairs - it re-materialises and re-uploads the
/// `.drv`. Negated from [`drv_present_predicate`] rather than restated, so the
/// two cannot drift: a `.drv` that is present but whose closure has a missing dependency is
/// deliberately NOT this, because re-evaluating cannot fetch that dependency, and
/// conflating the two burned an evaluation per stall and then failed it as
/// unrecoverable with the `.drv` cached the whole time.
pub fn drv_nar_absent_predicate(alias: &str) -> String {
    format!("NOT {}", drv_present_predicate(alias))
}

/// The shared build `{alias}`'s derivation has its full record in. Promotion and
/// dispatch require it: a shared build whose edges are not all recorded would
/// otherwise be queued as dependency-free.
pub fn walked_predicate(alias: &str) -> String {
    format!("EXISTS (SELECT 1 FROM derivation w WHERE w.id = {alias}.derivation AND w.walked)")
}

/// The statuses of a shared build that will still be built, and so still needs its
/// inputs. Closed under both promotion moves (`Created` to `Queued` and back), so a
/// promotion can never change whether a parent needs what it walks over.
pub const BUILDER_STATUSES: [BuildStatus; 4] = [
    BuildStatus::Created,
    BuildStatus::Queued,
    BuildStatus::Building,
    BuildStatus::FailedTransient,
];

/// The statuses at which the need flag decides whether an evaluation still waits: every
/// builder status plus `Skipped` and `Aborted`, which are thawed BY the need
/// returning. What a walk reaches is not this set but [`open_predicate`], which a
/// terminal-success shared build satisfies too while it cannot be fetched.
pub const NEED_BUILD_STATUSES: [BuildStatus; 6] = [
    BuildStatus::Created,
    BuildStatus::Queued,
    BuildStatus::Building,
    BuildStatus::FailedTransient,
    BuildStatus::Skipped,
    BuildStatus::Aborted,
];

/// Shared build `status`/`wanted` is work its evaluation is still waiting for: what
/// nothing needs is never promoted, so an evaluation that counts it as pending
/// waits forever (#666). Every terminal status is settled by definition, and a
/// shared build the dispatcher can still pick up - or already has - is work in flight
/// whatever the need flag says now: dispatch reads the status rather than the gate, and
/// a running build is deliberately left to finish.
///
/// The set is [`NEED_BUILD_STATUSES`], so a wanted `Skipped` shared build blocks and an
/// unwanted one does not, which is the whole meaning of the status. The need is
/// written before the thaw that follows it, and an evaluation settled in that gap
/// is settled over work it still owes.
pub fn blocks_evaluation(status: BuildStatus, wanted: bool) -> bool {
    if !NEED_BUILD_STATUSES.contains(&status) {
        return false;
    }

    wanted || matches!(status, BuildStatus::Queued | BuildStatus::Building)
}

/// [`blocks_evaluation`] as a predicate on shared build `{alias}`, so the decision has
/// one definition and the reader that asks the database for it cannot drift from
/// the reader that evaluates it in Rust.
pub fn blocks_evaluation_predicate(alias: &str) -> String {
    format!(
        "({alias}.status IN ({pending}) AND ({alias}.wanted OR {alias}.status IN ({in_flight})))",
        pending = crate::sql::status::build_in(&NEED_BUILD_STATUSES),
        in_flight = crate::sql::status::build_in(&[BuildStatus::Queued, BuildStatus::Building]),
    )
}

/// Shared build `{shared_build}` (its `derivation` row aliased `{walked}`) is a builder:
/// recorded, answered by the upstream probe, not a passthrough, and in a status an
/// evaluation will still have built. The one definition of what needs its
/// inputs and of what the adoption walk steps through, so the two can never
/// disagree.
///
/// `probed` is what makes "not a passthrough" a fact rather than a guess. The probe is
/// network and stays off every graph path, so a shared build is unprobed for as long as
/// a round takes; reading that as "will be built" needed the build closure of
/// every output an upstream serves, and the dispatcher hands those out inside the
/// window. The passthrough that follows withdraws the need, but a job already handed
/// to a worker keeps running to its end, and an input that cannot be fetched fails the
/// evaluation that no longer needed it.
pub fn builder_predicate(shared_build: &str, walked: &str) -> String {
    format!(
        "{walked}.walked AND {shared_build}.probed AND NOT {shared_build}.cache_available \
         AND {shared_build}.status IN ({pending})",
        pending = crate::sql::status::build_in(&BUILDER_STATUSES),
    )
}

/// Shared build `{alias}` is open: a parent cannot fetch it from our cache and no
/// verdict stands against it. Every walk reaches open shared builds and stops at the
/// rest, because what is below a fetchable shared build is served from our cache and
/// what is below a terminal failure is the requeue's to thaw. A terminal-success
/// shared build whose outputs are gone or whose closure has a missing dependency is open, and that is
/// the only way the missing dependency below it is reached at all; so is an aborted one, since
/// an abort is not a verdict and a live want is what thaws it.
pub fn open_predicate(alias: &str) -> String {
    format!(
        "(NOT {alias}.fetchable AND {alias}.status NOT IN ({failed}))",
        failed = crate::sql::status::build_in(&BuildStatus::TERMINAL_FAILURE),
    )
}

/// The shared build on derivation `{derivation}` still needs its inputs: it is no
/// passthrough. A failure walk stops at one that is, because a shared build available in a cache takes
/// finished bytes off an upstream cache: an input that can never build neither dooms it
/// nor reaches anything above it. Same rule as [`gates_predicate`]'s passthrough arm,
/// which lets a passthrough through with its `blocking_deps` unread, and as the refusal in
/// `graph::policy` to mark a failing passthrough `Permanent`.
pub fn non_passthrough_predicate(derivation: &str) -> String {
    format!(
        "NOT EXISTS (SELECT 1 FROM derivation_build rb \
         WHERE rb.derivation = {derivation} AND rb.cache_available)"
    )
}

/// Parents can get the outputs of shared build `{alias}` from OUR cache: it reached
/// terminal success and every output has a complete closure here. An upstream copy does not
/// count (#593): a parent of a shared build that is available in a cache and not yet passed through waits for the
/// passthrough, so a build pulls every input out of our own cache and the passthrough is on
/// the critical path exactly once, instead of every parent re-fetching from
/// the upstream cache itself. This is the one can-start fact a parent reads, and it
/// recurses over nothing: a dependency's own dependencies are already summarised
/// in its `blocking_deps`.
///
/// The `EXISTS` over `derivation_output` is load-bearing and not a tautology. The
/// `NOT EXISTS` under it is vacuously true for a shared build with NO output rows, so
/// without the guard a terminal-success shared build whose outputs were never recorded
/// reads as fetchable, stops counting toward its parents' `blocking_deps`, and
/// those parents are promoted and dispatched against an input nothing can
/// provide: the unbacked-output dead zone, measured on a live cluster.
pub fn fetchable_predicate(alias: &str) -> String {
    format!(
        "({alias}.status IN ({terminal_success}) AND {complete})",
        terminal_success = crate::sql::status::build_in(&BuildStatus::TERMINAL_SUCCESS),
        complete = shared_build_complete_predicate(alias),
    )
}

/// Every output of `{alias}` has its NAR in our cache.
///
/// The `EXISTS` is load-bearing and not a tautology. The `NOT EXISTS` under it is
/// vacuously true for a shared build with NO output rows, so without the guard a
/// terminal-success shared build whose outputs were never recorded reads as present,
/// stops counting toward its parents' `blocking_deps`, and those parents are
/// promoted and dispatched against an input nothing can provide: the
/// unbacked-output dead zone, measured on a live cluster.
pub fn present_predicate(alias: &str) -> String {
    format!(
        "(EXISTS (SELECT 1 FROM derivation_output o2 WHERE o2.derivation = {alias}.derivation) \
         AND NOT EXISTS (SELECT 1 FROM derivation_output o LEFT JOIN cached_path cp ON cp.hash = o.hash \
                         WHERE o.derivation = {alias}.derivation AND cp.file_hash IS NULL))"
    )
}

/// [`present_predicate`] as one probe of the outputs, for a projection: a
/// `RETURNING` pays both of the predicate's subplans per row, where a `WHERE`
/// would have semi-joined them. No output rows aggregate to NULL, which is absent.
pub fn present_value(alias: &str) -> String {
    format!(
        "coalesce((SELECT bool_and(cp.file_hash IS NOT NULL) FROM derivation_output o \
                   LEFT JOIN cached_path cp ON cp.hash = o.hash \
                   WHERE o.derivation = {alias}.derivation), false)"
    )
}

/// Present, and every runtime dependency leads to a complete shared build: the whole runtime
/// closure of `{alias}`'s outputs is in our cache. The counter is moved by
/// [`crate::graph::runtime_can_start`], never derived here, for the reason `blocking_deps`
/// is: a complete closure is transitive and a per-row predicate that looks one hop cannot
/// carry it.
pub fn shared_build_complete_predicate(alias: &str) -> String {
    format!(
        "({alias}.missing_runtime_deps = 0 AND {present})",
        present = present_predicate(alias),
    )
}

/// The gates a `Created` shared build must pass to be queued, minus its own status term:
/// walked, named by some evaluation, still needed, then one arm per kind of work.
/// A passthrough needs nothing more: it fetches finished bytes, so neither its inputs nor
/// its `.drv` matter. A build needs every input fetchable from our cache and its own
/// `.drv` importable.
///
/// The need is a column, not a walk: [`crate::graph::can_start::update_need`] rewrites it
/// on the events that change it and the consistency check rewrites it absolutely.
/// Reading it here is what makes it transitive, which the one-hop `EXISTS` this
/// replaced could not be: a `Created` parent counts as a builder, so the first
/// unwanted one re-wanted everything below it and a passed-through shared build's whole input
/// closure was built (#666). It also takes a correlated three-table subquery off
/// every promote, un-promote and sweep row.
///
/// `probed` is on the build arm for the reason it is on [`builder_predicate`], one level
/// up: a shared build reached over a RUNTIME dependency is needed whether or not anything will
/// build it, so it never passes through that predicate and arrives here unanswered. A
/// source FOD under a passed-through output is exactly that shape, and the dispatch loop
/// beats the probe tick: busybox's tarball was queued 60 ms after the round that
/// would have asked for it, built, and failed offline on an upstream cache that was
/// serving it. The passthrough arm needs no such term, since nothing is available in a cache
/// until the probe says so.
///
/// It must stay free of any reference to `{alias}`'s OWN `status`, which is why that
/// term lives in [`promotable_predicate`] instead. `m20260908_000002` and
/// `m20260909_000001` execute a demote and a promote in sequence in one transaction and
/// they cannot interfere only because this never reads the column the demote writes;
/// the same holds for [`crate::graph::can_start::repair_can_start`].
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

/// [`gates_predicate`] on a `Created` shared build: what promotion writes.
///
/// Dispatch does not re-derive this: it reads the status. So the invariant rests on
/// one rule, which every writer of `Queued` obeys in one of two ways: embed this
/// predicate in the write, or settle the rows just written with
/// [`crate::graph::can_start::unpromote_ungated`] in the same call.
/// [`crate::graph::can_start::repair_can_start`] is the backstop for a counter that drifted
/// under a lost move.
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

    /// A shared build nothing needs is never promoted, so an evaluation that waits
    /// for it never finishes: it is settled work, not pending work. `Queued` and
    /// `Building` are the exception in the other direction: the dispatcher reads
    /// the status, so both can still produce a build after the need is gone.
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

    /// The database asks the same question the Rust does, so the two must name the
    /// same statuses. Eval-done reads the predicate and nothing else now: a drift
    /// between them would settle an evaluation whose builds are still running,
    /// with no test failing on either side alone.
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

    /// The gate needs the `.drv` PRESENT, not complete. A `.drv` closure is trusted:
    /// the evaluation pushes it before it reports the derivation, and the one
    /// `.drv` state a fresh evaluation repairs is an absent NAR, which is why the
    /// gate and [`drv_nar_absent_predicate`] are exact negations of each other.
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

    /// The complete closure moved from the path to the shared build: present, with no runtime dependency
    /// on something that is not complete itself. `fetchable` is its projection onto
    /// a terminal-success status and reads no path counter at all.
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

    /// Fetchable is the one can-start fact parents read, and since #593 it is
    /// about OUR cache only: terminal success with every output complete. A shared build
    /// an upstream cache happens to serve is not fetchable until its passthrough lands.
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

    /// The `NOT EXISTS` over the outputs is vacuously true for a shared build with no
    /// output rows, so terminal success alone would backfill `fetchable` on one
    /// and stop it counting toward its parents' `blocking_deps`: the
    /// unbacked-output dead zone. The `EXISTS` guard is the fix and must stay.
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

    /// Both arms are gated by the need flag now. A build used to be promoted the moment its
    /// own inputs were fetchable, with nothing asking whether anything still wanted
    /// its output, which is why a passed-through shared build's whole input closure was built
    /// (#666). The term is a column read, so the one-hop EXISTS that used to leave
    /// every promote and un-promote is gone, and #591's gate work with it.
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

    /// `Skipped` is settled work: no gate acts on it, no walk steps THROUGH it and
    /// no evaluation waits for it. It is in none of the sets that decide any of
    /// those, and the thaw back to `Created` is what re-opens them at once. Being
    /// REACHED is the one thing it must still be, because the need returning is its
    /// thaw and a walk that cannot reach the row cannot write the column.
    #[test]
    fn skipped_is_neither_a_builder_nor_pending_nor_terminal() {
        assert!(NEED_BUILD_STATUSES.contains(&BuildStatus::Skipped));
        assert!(!BUILDER_STATUSES.contains(&BuildStatus::Skipped));
        assert!(!BuildStatus::PENDING.contains(&BuildStatus::Skipped));
        assert!(!BuildStatus::TERMINAL_SUCCESS.contains(&BuildStatus::Skipped));
        assert!(!BuildStatus::FAILURE.contains(&BuildStatus::Skipped));
        assert!(!BuildStatus::REQUEUEABLE.contains(&BuildStatus::Skipped));
    }

    /// The need arm reads a PARENT's status, which is only safe while both
    /// promotion moves stay inside the set it tests: a demote-then-promote pair in
    /// one transaction would otherwise change what the second statement's gate
    /// sees for an unrelated row.
    #[test]
    fn the_wanted_status_set_is_closed_under_both_promotion_moves() {
        for status in [BuildStatus::Created, BuildStatus::Queued] {
            assert!(
                BUILDER_STATUSES.contains(&status),
                "{status:?} is an endpoint of a promotion move"
            );
        }
    }

    /// The gate has two arms and the need sits outside both: a passthrough needs nothing
    /// more, a build needs fetchable inputs and an importable `.drv`.
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

    /// Promotable is a per-row check: walked, wanted, and then either the need (a
    /// passthrough) or zero blocking deps plus a present `.drv` (a build). Dispatchable is
    /// the same on Queued.
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

    /// The gates must not read the shared build's OWN `status`, or a demote-then-promote
    /// pair in one transaction (the can-start migrations, `can_start::repair_can_start`)
    /// starts double-moving rows: the demote writes the column the promote would read.
    #[test]
    fn the_gates_never_read_the_shared_builds_own_status_column() {
        let gates = gates_predicate("db");
        assert!(!gates.contains("db.status"), "{gates}");
        assert!(
            promotable_predicate("db").contains("db.status = 0 AND"),
            "the status term belongs to promotable alone"
        );
    }

    /// Open is the one reach condition of every walk: not fetchable, and no verdict
    /// against it. It reads two columns and no subquery, so a walk pays one row
    /// lookup per reached shared build; a terminal-success shared build whose closure has a
    /// missing dependency satisfies it like a builder does, and so does an aborted one.
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

    /// An abort is not a verdict. It stays in `REQUEUEABLE`, because an evaluation
    /// whose shared build sat aborted did not get it built, and it can be needed, because
    /// the need returning is what thaws it: twelve aborted shared builds inside a runtime
    /// closure were the wall behind 47 builders, and no requeue ever revisited them.
    #[test]
    fn an_aborted_shared_build_is_open_and_thawed_by_need() {
        assert!(NEED_BUILD_STATUSES.contains(&BuildStatus::Aborted));
        assert!(BuildStatus::REQUEUEABLE.contains(&BuildStatus::Aborted));
        assert!(!BuildStatus::TERMINAL_FAILURE.contains(&BuildStatus::Aborted));
        assert!(blocks_evaluation(BuildStatus::Aborted, true));
        assert!(!blocks_evaluation(BuildStatus::Aborted, false));
    }

    /// The walk carries the evaluation it walks for and the builder bit of the row
    /// it stands on, reaches open shared builds only, and steps out of a member over its
    /// runtime dependencies and out of a builder over every edge. One probe per member,
    /// so the two edge kinds are one index range scan and not two.
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
