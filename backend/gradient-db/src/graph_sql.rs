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
        assert!(cte.starts_with("WITH RECURSIVE dependents(derivation) AS"), "{cte}");
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
        assert!(cte.starts_with("WITH RECURSIVE closure(derivation) AS"), "{cte}");
        assert!(cte.contains("SELECT bj.derivation FROM build_job bj WHERE bj.evaluation = $1"), "{cte}");
        assert!(
            cte.contains("SELECT e.dependency FROM derivation_dependency e JOIN closure c ON e.derivation = c.derivation"),
            "must recurse toward dependencies: {cte}"
        );
    }

    /// The orphan-GC keep-set must be the build-dependency closure of the live
    /// roots (entry_points + build_jobs), not just the roots themselves - a dep
    /// reached only through `derivation_dependency` (its own `build_job` pruned
    /// with an old eval) must survive.
    #[test]
    fn reachable_cte_closes_over_roots_and_dependency_edges() {
        let cte = norm(&reachable_derivations_cte());
        assert!(cte.contains("FROM entry_point"), "entry points are roots: {cte}");
        assert!(cte.contains("FROM build_job"), "build_job derivations are roots: {cte}");
        assert!(
            cte.contains("SELECT e.dependency"),
            "recursion walks toward dependencies (the inputs a root needs): {cte}"
        );
    }
}
