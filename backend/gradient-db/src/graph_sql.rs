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

/// Dependency-readiness of anchor `{alias}`: every build dependency is
/// terminal-success AND `closure_complete`, or itself `substitutable`; and
/// every recorded input source is fully cached. This is THE readiness
/// definition - shared verbatim by promotion (`promote_ready`/
/// `promote_dependents`) and the dispatch gate (`find_ready_anchors`) so a
/// drift between them (a latent dead zone) is impossible by construction.
pub fn deps_ready_predicate(alias: &str) -> String {
    let terminal_success =
        crate::status_sql::build_in(&gradient_entity::build::BuildStatus::TERMINAL_SUCCESS);
    format!(
        r#"NOT EXISTS (
        SELECT 1 FROM derivation_dependency e
        LEFT JOIN derivation_build dep ON dep.derivation = e.dependency
        WHERE e.derivation = {alias}.derivation
          AND (dep.status IS NULL
               OR NOT (((dep.status IN ({terminal_success})) AND dep.closure_complete)
                       OR dep.substitutable)))
      AND NOT EXISTS (
        SELECT 1 FROM derivation_input_source s
        WHERE s.derivation = {alias}.derivation
          AND NOT EXISTS (
            SELECT 1 FROM cached_path cp
            WHERE cp.hash = s.hash AND cp.file_hash IS NOT NULL))"#
    )
}

/// The build target `{alias}`'s own `.drv` has its full NAR reference closure
/// backed in the cache (`cached_path.closure_complete`, whose ground truth is
/// every recorded `cached_path_reference` resolving to a backed, itself
/// closure-complete row). A `.drv` is an ordinary compressed-NAR store path, so
/// this is the authoritative "the worker can fetch and import the whole
/// input-`.drv` closure" signal - computed over the actual `.drv` NAR references,
/// not the eval-time build graph. The build-graph `drv_closure_cached` flag
/// mirrors it but diverges when eval pruning leaves dependency edges unrecorded
/// (`edges_complete = false` with no edges), dead-zoning a build whose `.drv`
/// closure is in fact fully cached; the dispatch gate accepts either signal.
pub fn drv_nar_closure_complete_predicate(alias: &str) -> String {
    format!(
        r#"EXISTS (
        SELECT 1 FROM derivation d
        JOIN cached_path cp ON cp.hash = d.hash
        WHERE d.id = {alias}.derivation AND cp.closure_complete)"#
    )
}

/// The build target `{alias}`'s own `.drv` NAR is not in our cache at all: no
/// `cached_path` row, or a row with no backing NAR. This is the only `.drv`
/// state a fresh evaluation repairs - it re-materialises and re-uploads the
/// `.drv`. Deliberately narrower than
/// `NOT drv_nar_closure_complete_predicate`, which is also true for a `.drv`
/// that is present and merely has an unconverged closure flag; re-evaluating
/// cannot set a flag, so conflating the two burned an evaluation per stall and
/// then failed it as unrecoverable with the `.drv` cached the whole time.
pub fn drv_nar_absent_predicate(alias: &str) -> String {
    format!(
        r#"NOT EXISTS (
        SELECT 1 FROM derivation d
        JOIN cached_path cp ON cp.hash = d.hash
        WHERE d.id = {alias}.derivation AND cp.file_hash IS NOT NULL)"#
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

    /// The readiness predicate must require terminal-success + closure_complete
    /// (or substitutable) on every dependency AND every input source cached -
    /// dropping either arm re-opens the InputsUnavailable poison class.
    #[test]
    fn readiness_predicate_gates_deps_and_input_sources() {
        let p = norm(&deps_ready_predicate("db"));
        let terminal_success =
            crate::status_sql::build_in(&gradient_entity::build::BuildStatus::TERMINAL_SUCCESS);
        assert!(
            p.contains(&format!(
                "(((dep.status IN ({terminal_success})) AND dep.closure_complete) OR dep.substitutable)"
            )),
            "deps must be terminal-success + closure_complete or substitutable: {p}"
        );
        assert!(
            p.contains("FROM derivation_input_source s") && p.contains("cp.file_hash IS NOT NULL"),
            "every input source must be fully cached: {p}"
        );
    }

    /// A `.drv` is an ordinary NAR store path, so the authoritative "the worker
    /// can import the whole input-`.drv` closure" signal is the `.drv`'s own
    /// `cached_path.closure_complete` (computed over real NAR references), not the
    /// eval-build-graph `drv_closure_cached` flag that diverges when pruning
    /// leaves edges unrecorded. The predicate must key on that.
    #[test]
    fn drv_nar_closure_predicate_keys_on_cached_path_closure_complete() {
        let p = norm(&drv_nar_closure_complete_predicate("db"));
        assert!(
            p.contains("JOIN cached_path cp ON cp.hash = d.hash")
                && p.contains("d.id = db.derivation")
                && p.contains("cp.closure_complete"),
            "must assert the build target's own .drv NAR-closure is complete: {p}"
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
