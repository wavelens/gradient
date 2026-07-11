/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The single definition of the recursive build-graph walk. Every traversal of
//! `derivation_dependency` (failure cascades, eval-closure sweeps, GC
//! reachability) is generated here so the walkers can never disagree on what
//! "reachable" means.

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
    let step = match direction {
        ClosureDirection::Dependencies => format!(
            "SELECT e.dependency FROM derivation_dependency e JOIN {name} c ON e.derivation = c.derivation"
        ),
        ClosureDirection::Dependents => format!(
            "SELECT e.derivation FROM derivation_dependency e JOIN {name} c ON e.dependency = c.derivation"
        ),
    };
    format!("WITH RECURSIVE {name}(derivation) AS ({seed_select} UNION {step})")
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

/// Closure of the derivations an evaluation directly references (its
/// `build_job` rows), walking toward dependencies. Binds the evaluation id as
/// `$1`. Shared by every per-eval sweep so they all see the same closure.
pub fn eval_closure_cte() -> String {
    dependency_closure_cte(
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
            cte.contains("SELECT e.derivation FROM derivation_dependency e JOIN dependents c ON e.dependency = c.derivation"),
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
            cte.contains("SELECT e.dependency FROM derivation_dependency e JOIN closure c ON e.derivation = c.derivation"),
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
            cte.contains("SELECT e.dependency"),
            "recursion walks toward dependencies (the inputs a root needs): {cte}"
        );
    }
}
