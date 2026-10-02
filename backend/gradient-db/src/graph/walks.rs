/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Postgres is estimating a recursive CTE at ten times the seed.
//! That estimate is two orders of magnitude too high for these walks.
//! Every recursive term is a `LATERAL` subquery behind an `OFFSET 0` fence for that reason.
//! The fence is leaving a nested loop with a per-row index lookup as the only legal plan.
//!
//! The set operator must stay `UNION`, which is deduplicating the frontier per iteration.
//! `UNION ALL` would make the walk exponential in depth on these diamond-heavy graphs.

use super::predicates::{builder_predicate, open_predicate};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr, TransactionTrait};

/// `SET LOCAL` is the only safe form, reverting with the transaction.
/// A bare `SET` would outlive the walk on a pooled connection.
/// The value must stay above the cluster's own `work_mem`, or the raise is a no-op.
pub const WALK_WORK_MEM: &str = "SET LOCAL work_mem = '64MB'";

pub async fn begin_walk<C>(db: &C) -> Result<DatabaseTransaction, DbErr>
where
    C: TransactionTrait<Transaction = DatabaseTransaction>,
{
    let txn = db.begin().await?;
    txn.execute_unprepared(WALK_WORK_MEM).await?;
    Ok(txn)
}

pub enum ClosureDirection {
    Dependencies,
    WantedBy,
}

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

pub fn dependency_closure_cte_body(
    name: &str,
    seed_select: &str,
    direction: ClosureDirection,
) -> String {
    bounded_dependency_closure_cte_body(name, seed_select, direction, "", None)
}

pub fn bounded_dependency_closure_cte_body(
    name: &str,
    seed_select: &str,
    direction: ClosureDirection,
    bound: &str,
    within: Option<&str>,
) -> String {
    let (probe, project) = match direction {
        ClosureDirection::Dependencies => ("e.derivation", "e.dependency"),
        ClosureDirection::WantedBy => ("e.dependency", "e.derivation"),
    };
    let restrict = if bound.is_empty() {
        String::new()
    } else {
        format!(" AND {bound}")
    };
    let step = lateral_step(
        name,
        "s.next",
        &format!(
            "SELECT {project} AS next FROM derivation_dependency e \
             WHERE {probe} = c.derivation{restrict}"
        ),
    );
    let step = within_step(step, "t.next", within);
    format!("{name}(derivation) AS ({seed_select} UNION {step})")
}

fn within_step(step: String, project: &str, within: Option<&str>) -> String {
    match within {
        None => step,
        Some(set) => format!(
            "SELECT {project} FROM ({step} OFFSET 0) t \
             WHERE EXISTS (SELECT 1 FROM {set} x WHERE x.derivation = t.next)"
        ),
    }
}

fn lateral_step(name: &str, project: &str, probe_select: &str) -> String {
    format!("SELECT {project} FROM {name} c, LATERAL ({probe_select} OFFSET 0) s")
}

pub fn runtime_closure_cte(name: &str, seed_select: &str) -> String {
    format!(
        "WITH RECURSIVE {}",
        runtime_closure_cte_body(name, seed_select)
    )
}

pub fn runtime_closure_cte_body(name: &str, seed_select: &str) -> String {
    bounded_dependency_closure_cte_body(
        name,
        seed_select,
        ClosureDirection::Dependencies,
        "e.kind IN (1, 2)",
        None,
    )
}

pub fn eval_closure_cte() -> String {
    format!("WITH RECURSIVE {}", eval_closure_cte_body())
}

pub fn eval_closure_cte_body() -> String {
    dependency_closure_cte_body(
        "closure",
        "SELECT bj.derivation FROM build_job bj WHERE bj.evaluation = $1",
        ClosureDirection::Dependencies,
    )
}

pub fn reachable_derivations_cte() -> String {
    format!("WITH RECURSIVE {}", reachable_derivations_cte_body())
}

pub fn reachable_derivations_cte_body() -> String {
    dependency_closure_cte_body(
        "reachable",
        "SELECT derivation FROM entry_point UNION SELECT derivation FROM build_job",
        ClosureDirection::Dependencies,
    )
}

pub fn live_cached_paths_cte() -> String {
    format!(
        "WITH RECURSIVE {reachable}, {runtime}, {kept}",
        reachable = reachable_derivations_cte_body(),
        runtime = runtime_closure_cte_body("runtime", "SELECT derivation FROM reachable"),
        kept = kept_hashes_cte_body("reachable", "runtime"),
    )
}

pub fn kept_hashes_cte_body(reachable: &str, runtime: &str) -> String {
    format!(
        "live(hash) AS (\
         SELECT o.hash FROM derivation_output o JOIN {runtime} t ON t.derivation = o.derivation \
         UNION \
         SELECT d.hash FROM derivation d JOIN {reachable} r ON r.derivation = d.id \
         UNION \
         SELECT s.hash FROM derivation_input_source s \
         JOIN {reachable} r ON r.derivation = s.derivation)"
    )
}

pub fn open_closure_cte(name: &str, seed_select: &str) -> String {
    format!(
        "WITH RECURSIVE {}",
        open_closure_cte_body(name, seed_select, None)
    )
}

pub fn open_closure_cte_body(name: &str, seed_select: &str, within: Option<&str>) -> String {
    let step = lateral_step(
        name,
        "c.evaluation, s.next, s.builder",
        &format!(
            "SELECT e.dependency AS next, ({builder}) AS builder \
             FROM derivation_dependency e \
             JOIN derivation_build dep ON dep.derivation = e.dependency \
             JOIN derivation w ON w.id = dep.derivation \
             WHERE e.derivation = c.derivation AND (c.builder OR e.kind IN (1, 2)) \
               AND {open}",
            builder = builder_predicate("dep", "w"),
            open = open_predicate("dep"),
        ),
    );
    let step = within_step(step, "t.evaluation, t.next, t.builder", within);
    format!("{name}(evaluation, derivation, builder) AS ({seed_select} UNION {step})")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn norm(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn live_cached_paths_close_reachable_outputs_and_drvs_over_references() {
        let cte = norm(&live_cached_paths_cte());

        assert!(
            cte.starts_with("WITH RECURSIVE reachable(derivation) AS ("),
            "{cte}"
        );
        assert!(
            cte.contains(
                "runtime(derivation) AS (SELECT derivation FROM reachable UNION SELECT s.next"
            ),
            "{cte}"
        );
        assert!(
            cte.contains(concat!(
                "live(hash) AS (SELECT o.hash FROM derivation_output o ",
                "JOIN runtime t ON t.derivation = o.derivation UNION ",
                "SELECT d.hash FROM derivation d JOIN reachable r ON r.derivation = d.id UNION ",
                "SELECT s.hash FROM derivation_input_source s ",
                "JOIN reachable r ON r.derivation = s.derivation)",
            )),
            "{cte}"
        );
    }

    #[tokio::test]
    async fn the_walk_raises_work_mem_with_set_local_inside_its_own_transaction() {
        assert!(WALK_WORK_MEM.starts_with("SET LOCAL "), "{WALK_WORK_MEM}");

        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();
        begin_walk(&db)
            .await
            .expect("the walk opens")
            .commit()
            .await
            .expect("the walk closes");

        let log = crate::pool::statements(db.into_transaction_log());
        assert_eq!(
            log.len(),
            1,
            "one statement, bracketed by the walk: {log:?}"
        );
        assert!(log[0].contains(WALK_WORK_MEM), "{log:?}");
    }

    #[test]
    fn wanted_by_walk_upward() {
        let cte = norm(&dependency_closure_cte(
            "wanted_by",
            "SELECT $1::uuid",
            ClosureDirection::WantedBy,
        ));
        assert!(
            cte.starts_with("WITH RECURSIVE wanted_by(derivation) AS"),
            "{cte}"
        );
        assert!(
            cte.contains(
                "SELECT e.derivation AS next FROM derivation_dependency e WHERE e.dependency = c.derivation"
            ),
            "must walk parents upward via the dependency edge: {cte}"
        );
    }

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

    #[test]
    fn every_recursive_term_is_fenced_into_a_nested_loop() {
        for cte in [
            norm(&eval_closure_cte()),
            norm(&reachable_derivations_cte()),
            norm(&dependency_closure_cte(
                "wanted_by",
                "SELECT $1::uuid",
                ClosureDirection::WantedBy,
            )),
            norm(&runtime_closure_cte("refs", "SELECT $1::uuid")),
            norm(&open_closure_cte(
                "pending",
                "SELECT $1::uuid, $2::uuid, true",
            )),
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

    #[test]
    fn a_bounded_walk_restricts_inside_the_probe() {
        let cte = norm(&bounded_dependency_closure_cte_body(
            "wanted_by",
            "SELECT $1::uuid",
            ClosureDirection::WantedBy,
            "e.kind IN (1, 2)",
            None,
        ));

        let probe = "WHERE e.dependency = c.derivation AND e.kind IN (1, 2) OFFSET 0) s";

        assert!(
            cte.contains(probe),
            "the bound belongs inside the fenced probe: {cte}"
        );
    }

    #[test]
    fn a_walk_within_a_closure_semi_joins_it_once_per_level() {
        let cte = norm(&bounded_dependency_closure_cte_body(
            "wanted_by",
            "SELECT $1::uuid",
            ClosureDirection::WantedBy,
            "e.kind IN (1, 2)",
            Some("closure"),
        ));

        assert!(
            cte.contains(
                "AND e.kind IN (1, 2) OFFSET 0) s OFFSET 0) t \
                 WHERE EXISTS (SELECT 1 FROM closure x WHERE x.derivation = t.next))"
            ),
            "the closure test sits outside the fenced probe: {cte}"
        );
        assert!(
            !cte.contains("IN (SELECT derivation FROM closure)"),
            "no closure membership is a subplan: {cte}"
        );
    }

    #[test]
    fn the_runtime_closure_walks_runtime_dependencies_only() {
        let cte = norm(&runtime_closure_cte("eval_paths", "SELECT $1::uuid"));

        assert!(
            cte.starts_with("WITH RECURSIVE eval_paths(derivation) AS"),
            "{cte}"
        );
        assert!(
            cte.contains(
                "SELECT e.dependency AS next FROM derivation_dependency e \
                 WHERE e.derivation = c.derivation AND e.kind IN (1, 2) OFFSET 0) s"
            ),
            "{cte}"
        );
    }

    #[test]
    fn the_live_set_walks_runtime_dependencies_from_live_derivations_and_keeps_their_sources() {
        let cte = norm(&live_cached_paths_cte());
        assert!(cte.contains("e.kind IN (1, 2)"), "{cte}");
        assert!(
            cte.contains("SELECT s.hash FROM derivation_input_source s JOIN"),
            "{cte}"
        );
        assert!(!cte.contains("cached_path_reference"), "{cte}");
    }

    #[test]
    fn an_incomplete_terminal_shared_build_is_reached_and_stepped_through() {
        let cte = norm(&open_closure_cte(
            "pending",
            "SELECT $1::uuid, $2::uuid, true",
        ));
        assert!(
            !cte.contains("build_job"),
            "a name is what adoption writes for what the walk reaches: {cte}"
        );
        assert!(
            cte.contains(&format!("AND {} OFFSET 0) s", norm(&open_predicate("dep")))),
            "reach is open, never a status list: {cte}"
        );
        assert!(
            !cte.contains("AND dep.status IN (0, 1, 2, 8, 10)"),
            "the old reach set must not come back: {cte}"
        );
    }

    #[test]
    fn a_walk_within_a_set_semi_joins_it_once_per_level() {
        let cte = norm(&open_closure_cte_body(
            "wanted",
            "SELECT NULL::uuid, r.derivation, r.builder FROM region r",
            Some("region"),
        ));
        assert!(
            cte.contains(&format!(
                "AND {} OFFSET 0) s OFFSET 0) t \
                 WHERE EXISTS (SELECT 1 FROM region x WHERE x.derivation = t.next))",
                norm(&open_predicate("dep"))
            )),
            "{cte}"
        );
        assert!(
            norm(&open_closure_cte_body("x", "SELECT 1", None)).ends_with(&format!(
                "AND {} OFFSET 0) s)",
                norm(&open_predicate("dep"))
            )),
            "no set is the unrestricted walk"
        );
    }
}
