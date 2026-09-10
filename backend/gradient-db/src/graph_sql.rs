/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The single definition of the recursive graph walks. Every traversal of
//! `derivation_dependency` (failure cascades, eval-closure sweeps, GC
//! reachability) and of `cached_path_reference` (NAR reference closures) is
//! generated here so the walkers can never disagree on what "reachable" means,
//! and so the join shape below has exactly one place to live.
//!
//! Postgres estimates a recursive CTE's working table at ten times the seed,
//! which for these walks overshoots by two orders of magnitude (348,870
//! estimated against 2,439 actual on a 44k-node eval closure). At that
//! cardinality a merge join against the whole edge index costs out cheaper than
//! a nested loop, so the planner rescans all four million edges once per
//! iteration. Every recursive term here is therefore written as a `LATERAL`
//! subquery with an `OFFSET 0` optimisation fence: the fence stops the planner
//! pulling the subquery back up, which leaves a nested loop with a per-row index
//! lookup as the only legal plan. Measured on production: eval closure 5,278 ms
//! to 955 ms, GC keep-set 40,069 ms to 9,746 ms, the `cached_path_reference`
//! walk from over 180,000 ms to 18,425 ms.
//!
//! The set operator stays `UNION`. It is what deduplicates the frontier on each
//! iteration, and these graphs are diamond-heavy enough that the dependents walk
//! already emits 940k rows for 68k distinct nodes; `UNION ALL` would drop the
//! deduplication and make the walk exponential in depth.

pub enum ClosureDirection {
    /// Walk from the roots toward the inputs they need (the build-time closure).
    Dependencies,
    /// Walk from the roots toward the anchors that need them (the dependents).
    Dependents,
}

/// A `WITH RECURSIVE {name}(derivation) AS (...)` prelude closing `seed_select`
/// over `derivation_dependency` in `direction`. The seed may contain UNION arms;
/// every arm must select exactly one derivation-id column.
pub fn dependency_closure_cte(
    name: &str,
    seed_select: &str,
    direction: ClosureDirection,
) -> String {
    format!(
        "WITH RECURSIVE {}",
        dependency_closure_cte_body(name, seed_select, direction)
    )
}

/// The bare `{name}(derivation) AS (...)` CTE body, without the `WITH RECURSIVE`
/// prefix, so a statement can bind several closures under one `WITH RECURSIVE`
/// (e.g. an eval closure plus the deterministic-blocked set it constrains).
pub fn dependency_closure_cte_body(
    name: &str,
    seed_select: &str,
    direction: ClosureDirection,
) -> String {
    bounded_dependency_closure_cte_body(name, seed_select, direction, "")
}

/// A closure walk confined to a set another CTE in the same statement already
/// binds (an eval closure, a requeue candidate set). `bound` is an extra
/// predicate over the edge alias `e`, applied inside the lateral probe so it
/// prunes at the index lookup rather than after the join; an empty `bound` is
/// the unrestricted walk.
pub fn bounded_dependency_closure_cte_body(
    name: &str,
    seed_select: &str,
    direction: ClosureDirection,
    bound: &str,
) -> String {
    let (probe, project) = match direction {
        ClosureDirection::Dependencies => ("e.derivation", "e.dependency"),
        ClosureDirection::Dependents => ("e.dependency", "e.derivation"),
    };
    let restrict = if bound.is_empty() {
        String::new()
    } else {
        format!(" AND {bound}")
    };
    format!(
        "{name}(derivation) AS ({seed_select} UNION {})",
        lateral_step(
            name,
            &format!(
                "SELECT {project} AS next FROM derivation_dependency e \
                 WHERE {probe} = c.derivation{restrict}"
            ),
        )
    )
}

/// One fenced recursive term: join the working table `{name}` (aliased `c`) to
/// `probe_select` through a `LATERAL` subquery that `OFFSET 0` keeps the planner
/// from pulling up. See the module docs for why the fence is load-bearing rather
/// than decorative. `probe_select` projects a single column aliased `next` and
/// correlates to the working-table row through `c`.
fn lateral_step(name: &str, probe_select: &str) -> String {
    format!("SELECT s.next FROM {name} c, LATERAL ({probe_select} OFFSET 0) s")
}

/// A `WITH RECURSIVE {name}(hash) AS (...)` prelude closing `seed_select` over
/// `cached_path_reference`, walking from a referrer to the store hashes it
/// references. This is the NAR-level closure (what a client must fetch), as
/// opposed to the build-time closure over `derivation_dependency`.
pub fn reference_closure_cte(name: &str, seed_select: &str) -> String {
    format!(
        "WITH RECURSIVE {}",
        reference_closure_cte_body(name, seed_select)
    )
}

/// The bare `{name}(hash) AS (...)` reference-closure body, for statements that
/// bind it as a prelude to an UPDATE or alongside another CTE.
pub fn reference_closure_cte_body(name: &str, seed_select: &str) -> String {
    format!(
        "{name}(hash) AS ({seed_select} UNION {})",
        lateral_step(
            name,
            "SELECT r.reference_hash AS next FROM cached_path_reference r WHERE r.referrer = c.hash",
        )
    )
}

/// The build target `{alias}`'s own `.drv` is whole: its NAR is stored and every
/// reference counted by `cached_path.missing_references` resolves. A `.drv` is an
/// ordinary compressed-NAR store path, so this is the authoritative "the worker
/// can fetch and import the whole input-`.drv` closure" signal - computed over the
/// actual `.drv` NAR references, not the eval-time build graph. It is a term of
/// [`gates_predicate`], so a build-graph mirror of it would only diverge from the
/// NAR ground truth when eval pruning leaves a dependency unwalked, and dead-zone
/// a build whose `.drv` closure is in fact fully cached.
pub fn drv_whole_predicate(alias: &str) -> String {
    format!(
        r#"EXISTS (
        SELECT 1 FROM derivation d
        JOIN cached_path cp ON cp.hash = d.hash
        WHERE d.id = {alias}.derivation AND {whole})"#,
        whole = crate::nar_closure::whole_predicate("cp"),
    )
}

/// The build target `{alias}`'s own `.drv` NAR is not in our cache at all: no
/// `cached_path` row, or a row with no backing NAR. This is the only `.drv`
/// state a fresh evaluation repairs - it re-materialises and re-uploads the
/// `.drv`. Deliberately narrower than
/// `NOT drv_whole_predicate`, which is also true for a `.drv` that is present
/// and merely misses a reference; re-evaluating cannot fetch that reference, so
/// conflating the two burned an evaluation per stall and then failed it as
/// unrecoverable with the `.drv` cached the whole time.
pub fn drv_nar_absent_predicate(alias: &str) -> String {
    format!(
        r#"NOT EXISTS (
        SELECT 1 FROM derivation d
        JOIN cached_path cp ON cp.hash = d.hash
        WHERE d.id = {alias}.derivation AND cp.file_hash IS NOT NULL)"#
    )
}

/// The anchor `{alias}`'s derivation has its full record in. Promotion and
/// dispatch require it: an anchor whose edges are not all recorded would
/// otherwise be queued as dependency-free.
pub fn walked_predicate(alias: &str) -> String {
    format!("EXISTS (SELECT 1 FROM derivation w WHERE w.id = {alias}.derivation AND w.walked)")
}

/// Dependents can get the outputs of anchor `{alias}`: an upstream serves them,
/// or the anchor succeeded and every output is whole in our cache. This is the one
/// readiness fact a dependent reads, and it recurses over nothing - a
/// dependency's own dependencies are already summarised in its `unready_deps`.
///
/// The `EXISTS` over `derivation_output` is load-bearing and not a tautology. The
/// `NOT EXISTS` under it is vacuously true for an anchor with NO output rows, so
/// without the guard a terminal-success anchor whose outputs were never recorded
/// reads as fetchable, stops counting toward its dependents' `unready_deps`, and
/// those dependents are promoted and dispatched against an input nothing can
/// provide: the unbacked-output dead zone, measured on a live cluster. An anchor
/// with no outputs is therefore NOT fetchable until its outputs are recorded.
/// `m20260908_000002`'s frozen copy carries the same guard, and the two must agree
/// or the backfill and the first ripple disagree about the same row.
pub fn fetchable_predicate(alias: &str) -> String {
    format!(
        r#"({alias}.substitutable
    OR ({alias}.status IN ({terminal_success})
        AND EXISTS (SELECT 1 FROM derivation_output o2 WHERE o2.derivation = {alias}.derivation)
        AND NOT EXISTS (
            SELECT 1 FROM derivation_output o
            LEFT JOIN cached_path cp ON cp.hash = o.hash
            WHERE o.derivation = {alias}.derivation AND NOT {whole})))"#,
        terminal_success =
            crate::status_sql::build_in(&gradient_entity::build::BuildStatus::TERMINAL_SUCCESS),
        whole = crate::nar_closure::whole_predicate("cp"),
    )
}

/// The gates a `Created` anchor must pass to be queued, minus the status term:
/// walked, no unready dependency, wanted by some evaluation, and its own `.drv`
/// importable unless an upstream serves it.
///
/// It must stay free of any reference to `status`, which is why the term lives in
/// [`promotable_predicate`] instead. `m20260908_000002` runs its demote and its
/// promote in sequence in one transaction and they cannot interfere only because
/// this never reads the column the demote writes; the same holds for
/// `readiness::repair_pending`. A status term migrating in here starts
/// double-moving rows, silently.
pub fn gates_predicate(alias: &str) -> String {
    format!(
        r#"({walked}
    AND {alias}.unready_deps = 0
    AND EXISTS (SELECT 1 FROM build_job bj WHERE bj.derivation = {alias}.derivation)
    AND ({alias}.substitutable OR {drv_whole}))"#,
        walked = walked_predicate(alias),
        drv_whole = drv_whole_predicate(alias),
    )
}

/// [`gates_predicate`] on a `Created` anchor: what promotion writes, and the whole
/// of what makes `Queued` mean the gates held. Dispatch does not re-derive this; it
/// reads the status, so the promotion write and the matching un-promote
/// ([`crate::readiness::unpromote_ungated`]) are the only two things maintaining the
/// invariant, with [`crate::readiness::repair_pending`] as the backstop for a counter
/// that drifted.
pub fn promotable_predicate(alias: &str) -> String {
    format!(
        "({alias}.status = {created} AND {gates})",
        created = crate::status_sql::build(gradient_entity::build::BuildStatus::Created),
        gates = gates_predicate(alias),
    )
}

/// Closure of the derivations an evaluation directly references (its
/// `build_job` rows), walking toward dependencies. Binds the evaluation id as
/// `$1`. Shared by every per-eval sweep so they all see the same closure.
pub fn eval_closure_cte() -> String {
    format!("WITH RECURSIVE {}", eval_closure_cte_body())
}

/// The eval-closure CTE body (no `WITH RECURSIVE` prefix), for statements that
/// bind it alongside a second closure under one `WITH RECURSIVE`.
pub fn eval_closure_cte_body() -> String {
    dependency_closure_cte_body(
        "closure",
        "SELECT bj.derivation FROM build_job bj WHERE bj.evaluation = $1",
        ClosureDirection::Dependencies,
    )
}

/// Build-dependency closure of the live GC roots (`entry_point` and `build_job`
/// derivations). A derivation in this set is still needed to build or serve a
/// retained closure and must never be reclaimed, even with no `build_job` of
/// its own: `build_job` rows are pruned with old evals while dependency edges
/// and anchors persist.
pub fn reachable_derivations_cte() -> String {
    dependency_closure_cte(
        "reachable",
        "SELECT derivation FROM entry_point UNION SELECT derivation FROM build_job",
        ClosureDirection::Dependencies,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn norm(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// Dependents direction must walk upward (a dependency edge leads to the
    /// anchors that consume it) so failure cascades reach every consumer.
    #[test]
    fn dependents_walk_upward() {
        let cte = norm(&dependency_closure_cte(
            "dependents",
            "SELECT $1::uuid",
            ClosureDirection::Dependents,
        ));
        assert!(
            cte.starts_with("WITH RECURSIVE dependents(derivation) AS"),
            "{cte}"
        );
        assert!(
            cte.contains(
                "SELECT e.derivation AS next FROM derivation_dependency e WHERE e.dependency = c.derivation"
            ),
            "must walk dependents upward via the dependency edge: {cte}"
        );
    }

    /// Dependencies direction must walk downward (toward inputs) so keep-sets
    /// and per-eval sweeps cover the full build-time closure.
    #[test]
    fn dependencies_walk_downward() {
        let cte = norm(&eval_closure_cte());
        assert!(
            cte.starts_with("WITH RECURSIVE closure(derivation) AS"),
            "{cte}"
        );
        assert!(
            cte.contains("SELECT bj.derivation FROM build_job bj WHERE bj.evaluation = $1"),
            "{cte}"
        );
        assert!(
            cte.contains(
                "SELECT e.dependency AS next FROM derivation_dependency e WHERE e.derivation = c.derivation"
            ),
            "must recurse toward dependencies: {cte}"
        );
    }

    /// A `.drv` is an ordinary NAR store path, so the authoritative "the worker
    /// can import the whole input-`.drv` closure" signal is the `.drv` row's own
    /// reference counter (computed over real NAR references), not an
    /// eval-build-graph mirror of it that diverges when pruning leaves a
    /// dependency unwalked. The predicate must key on that.
    #[test]
    fn drv_whole_predicate_reads_the_reference_counter() {
        let p = norm(&drv_whole_predicate("db"));
        assert!(
            p.contains("JOIN cached_path cp ON cp.hash = d.hash")
                && p.contains("d.id = db.derivation")
                && p.contains("cp.file_hash IS NOT NULL")
                && p.contains("cp.missing_references = 0"),
            "must assert the build target's own .drv row is whole: {p}"
        );
    }

    /// Fetchable is the one readiness fact dependents read: an upstream copy, or
    /// terminal success with every output whole. No recursion over deps.
    #[test]
    fn fetchable_reads_upstream_or_whole_outputs_and_nothing_recursive() {
        let p = norm(&fetchable_predicate("db"));
        assert!(
            p.starts_with("(db.substitutable OR (db.status IN (3, 7)"),
            "{p}"
        );
        assert!(
            p.contains("NOT (cp.file_hash IS NOT NULL AND cp.missing_references = 0)"),
            "{p}"
        );
        assert!(
            !p.contains("derivation_dependency"),
            "no walk over the build graph: {p}"
        );
    }

    /// The `NOT EXISTS` over the outputs is vacuously true for an anchor with no
    /// output rows, so terminal success alone would backfill `fetchable` on one
    /// and stop it counting toward its dependents' `unready_deps` - the
    /// unbacked-output dead zone. The `EXISTS` guard is the fix and must stay.
    #[test]
    fn an_anchor_with_no_outputs_is_not_fetchable() {
        let p = norm(&fetchable_predicate("db"));
        assert!(
            p.contains(
                "AND EXISTS (SELECT 1 FROM derivation_output o2 WHERE o2.derivation = db.derivation) AND NOT EXISTS"
            ),
            "the output guard must precede the NOT EXISTS: {p}"
        );
    }

    /// Promotable is a per-row check: walked, zero unready deps, wanted, and a
    /// whole `.drv` (or an upstream copy). Dispatchable is the same on Queued.
    #[test]
    fn promotable_is_created_plus_the_gates() {
        let p = norm(&promotable_predicate("db"));
        assert!(p.starts_with("(db.status = 0 AND ("), "{p}");
        assert!(p.contains("w.walked"), "{p}");
        assert!(p.contains("db.unready_deps = 0"), "{p}");
        assert!(
            p.contains("FROM build_job bj WHERE bj.derivation = db.derivation"),
            "{p}"
        );
        assert!(p.contains("db.substitutable OR EXISTS"), "{p}");
        assert!(
            !p.contains("derivation_input_source"),
            "sources are references of the .drv: {p}"
        );
    }

    /// The gates must not read `status`, or a demote-then-promote pair in one
    /// transaction (the readiness migration, `readiness::repair_pending`) starts
    /// double-moving rows: the demote writes the column the promote would read.
    #[test]
    fn the_gates_never_read_the_status_column() {
        let gates = gates_predicate("db");
        assert!(!gates.contains("status"), "{gates}");
        assert!(
            promotable_predicate("db").contains("db.status = 0 AND"),
            "the status term belongs to promotable alone"
        );
    }

    /// Every promotion and dispatch gate reads the derivation's `walked` bit
    /// through this one predicate.
    #[test]
    fn walked_predicate_reads_the_derivation_row() {
        let p = norm(&walked_predicate("db"));
        assert_eq!(
            p,
            "EXISTS (SELECT 1 FROM derivation w WHERE w.id = db.derivation AND w.walked)"
        );
    }

    /// The orphan-GC keep-set must be the build-dependency closure of the live
    /// roots (entry_points + build_jobs), not just the roots themselves - a dep
    /// reached only through `derivation_dependency` (its own `build_job` pruned
    /// with an old eval) must survive.
    #[test]
    fn reachable_cte_closes_over_roots_and_dependency_edges() {
        let cte = norm(&reachable_derivations_cte());
        assert!(
            cte.contains("FROM entry_point"),
            "entry points are roots: {cte}"
        );
        assert!(
            cte.contains("FROM build_job"),
            "build_job derivations are roots: {cte}"
        );
        assert!(
            cte.contains("SELECT e.dependency AS next"),
            "recursion walks toward dependencies (the inputs a root needs): {cte}"
        );
    }

    /// The fence is the whole performance fix: without `LATERAL (... OFFSET 0)`
    /// the planner takes the recursive CTE's 10x working-table estimate at face
    /// value and merge-joins the entire edge table once per iteration. Assert it
    /// on every generated walk so a later tidy-up cannot quietly drop it.
    #[test]
    fn every_recursive_term_is_fenced_into_a_nested_loop() {
        for cte in [
            norm(&eval_closure_cte()),
            norm(&reachable_derivations_cte()),
            norm(&dependency_closure_cte(
                "dependents",
                "SELECT $1::uuid",
                ClosureDirection::Dependents,
            )),
            norm(&reference_closure_cte("refs", "SELECT $1::text")),
        ] {
            assert!(
                cte.contains("LATERAL ("),
                "recursive term must be lateral: {cte}"
            );
            assert!(
                cte.contains("OFFSET 0) s"),
                "lateral probe must be fenced: {cte}"
            );
            assert!(
                !cte.contains("UNION ALL"),
                "UNION dedupes the frontier; UNION ALL is exponential on a diamond graph: {cte}"
            );
        }
    }

    /// A bounded walk must apply its restriction inside the lateral probe and
    /// ahead of the fence, so the frontier is pruned at the index lookup rather
    /// than after the join, and the `OFFSET 0` still terminates the subquery.
    #[test]
    fn a_bounded_walk_restricts_inside_the_probe() {
        let cte = norm(&bounded_dependency_closure_cte_body(
            "dependents",
            "SELECT $1::uuid",
            ClosureDirection::Dependents,
            "e.derivation IN (SELECT derivation FROM closure)",
        ));

        let probe = "WHERE e.dependency = c.derivation \
                     AND e.derivation IN (SELECT derivation FROM closure) OFFSET 0) s";

        assert!(
            cte.contains(probe),
            "the bound belongs inside the fenced probe: {cte}"
        );
    }

    /// The reference closure walks NAR references (what a client must fetch),
    /// not build inputs, so it keys on `cached_path_reference` and carries a
    /// `hash` column rather than a `derivation` one.
    #[test]
    fn reference_closure_walks_cached_path_reference_by_referrer() {
        let cte = norm(&reference_closure_cte(
            "eval_paths",
            "SELECT $1::text AS hash",
        ));

        assert!(
            cte.starts_with("WITH RECURSIVE eval_paths(hash) AS"),
            "{cte}"
        );
        assert!(
            cte.contains(
                "SELECT r.reference_hash AS next FROM cached_path_reference r WHERE r.referrer = c.hash"
            ),
            "must walk referrer to referenced hash: {cte}"
        );
    }
}
