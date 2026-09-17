/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Applies one query's budget to what its plan actually did. `relation_rows` is
//! `pg_class.reltuples` per relation, which is what makes a sequential scan of a
//! six-row lookup table legal and one of half a million rows a failure.
//!
//! The rules step aside where they would measure the wrong thing. A sequential
//! scan that keeps what it read is the planner reading a table it needs in full,
//! not a missing index. A plan that aggregates reads many rows to return one by
//! definition, a statement that returned nothing has no denominator, and a batch
//! is judged against the values it was handed. What such a statement cost is
//! still bounded, by buffers.

use std::collections::HashMap;

use super::{Budget, Measured, Shape, Spill, Violation};

pub fn check(
    measured: &Measured,
    budget: &Budget,
    relation_rows: &HashMap<String, u64>,
) -> Vec<Violation> {
    let mut out = Vec::new();

    if let Some(limit) = budget.seq_scan_rows {
        for scan in &measured.seq_scans {
            let size = relation_rows
                .get(&scan.relation)
                .copied()
                .unwrap_or_default();
            if size > limit && scan.removed > scan.read.saturating_sub(scan.removed) {
                out.push(Violation {
                    rule: "seq_scan",
                    detail: format!(
                        "Seq Scan on {} ({size} rows, limit {limit}) dropped {} of the {} it read",
                        scan.relation, scan.removed, scan.read,
                    ),
                    fatal: true,
                });
            }
        }
    }

    if measured.buffers > budget.buffers {
        out.push(Violation {
            rule: "buffers",
            detail: format!("buffers {} > {}", measured.buffers, budget.buffers),
            fatal: true,
        });
    }

    let asked_for = measured.rows_out.max(measured.inputs).max(1);
    let amplification = measured.rows_scanned / asked_for;
    if ratio_applies(measured) && amplification > budget.amplification {
        out.push(Violation {
            rule: "amplification",
            detail: format!(
                "{} rows scanned per {asked_for} returned or bound \
                 ({amplification}x, limit {}x)",
                measured.rows_scanned, budget.amplification,
            ),
            fatal: true,
        });
    }

    if ratio_applies(measured) && measured.worst_filtered.1 > budget.rows_removed {
        out.push(Violation {
            rule: "rows_removed",
            detail: format!(
                "{} removed by filter on {} (limit {})",
                measured.worst_filtered.1, measured.worst_filtered.0, budget.rows_removed,
            ),
            fatal: true,
        });
    }

    if measured.max_loops > budget.loops {
        out.push(Violation {
            rule: "loops",
            detail: format!("{} loops (limit {})", measured.max_loops, budget.loops),
            fatal: true,
        });
    }

    if measured.spilled {
        out.push(Violation {
            rule: "spill",
            detail: "a sort or hash spilled to disk".to_string(),
            fatal: budget.spill == Spill::Forbidden,
        });
    }

    for shape in budget
        .shape
        .iter()
        .filter(|_| !measured.fenced_types.is_empty())
    {
        match shape {
            Shape::Require(node) if !measured.fenced_types.iter().any(|n| n == node) => {
                out.push(Violation {
                    rule: "shape_required",
                    detail: format!("the recursive term lost its {node}"),
                    fatal: true,
                });
            }

            Shape::Forbid(node) if measured.fenced_types.iter().any(|n| n == node) => {
                out.push(Violation {
                    rule: "shape_forbidden",
                    detail: format!("the recursive term contains a {node}"),
                    fatal: true,
                });
            }

            _ => {}
        }
    }

    if budget.reason.is_some() && measured.buffers * 2 < budget.buffers {
        out.push(Violation {
            rule: "stale_override",
            detail: format!(
                "override allows {} buffers, the query used {}; delete the override",
                budget.buffers, measured.buffers,
            ),
            fatal: false,
        });
    }

    out
}

/// Whether rows read per row asked for says anything about this plan.
fn ratio_applies(measured: &Measured) -> bool {
    (measured.rows_out > 0 || measured.inputs > 0) && !measured.collapses
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::check;
    use crate::sql::{Budget, Measured, Scan, Shape};

    fn scan(relation: &str, read: u64, removed: u64) -> Scan {
        Scan {
            relation: relation.to_string(),
            read,
            removed,
        }
    }

    fn rows(pairs: &[(&str, u64)]) -> HashMap<String, u64> {
        pairs.iter().map(|(t, n)| ((*t).to_string(), *n)).collect()
    }

    fn clean() -> Measured {
        Measured {
            buffers: 10,
            rows_out: 5,
            rows_scanned: 20,
            max_loops: 1,
            ..Default::default()
        }
    }

    #[test]
    fn a_clean_plan_has_no_violations() {
        assert!(check(&clean(), &Budget::HOT, &rows(&[])).is_empty());
    }

    #[test]
    fn a_big_table_seq_scan_that_throws_its_read_away_is_fatal_on_hot() {
        let m = Measured {
            seq_scans: vec![scan("derivation", 500_000, 499_990)],
            ..clean()
        };

        let v = check(&m, &Budget::HOT, &rows(&[("derivation", 500_000)]));
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].rule, "seq_scan");
        assert!(v[0].fatal);
    }

    /// The planner reading a table it needs in full is the right plan, not a
    /// missing index.
    #[test]
    fn a_seq_scan_that_keeps_what_it_read_is_fine() {
        let m = Measured {
            seq_scans: vec![scan("derivation", 500_000, 0)],
            ..clean()
        };

        assert!(check(&m, &Budget::HOT, &rows(&[("derivation", 500_000)])).is_empty());
    }

    #[test]
    fn a_small_table_seq_scan_is_fine() {
        let m = Measured {
            seq_scans: vec![scan("role", 6, 6)],
            ..clean()
        };

        assert!(check(&m, &Budget::HOT, &rows(&[("role", 6)])).is_empty());
    }

    #[test]
    fn sweep_permits_any_seq_scan() {
        let m = Measured {
            seq_scans: vec![scan("derivation", 500_000, 500_000)],
            ..clean()
        };

        assert!(check(&m, &Budget::SWEEP, &rows(&[("derivation", 500_000)])).is_empty());
    }

    #[test]
    fn amplification_counts_rows_per_row_returned() {
        let m = Measured {
            rows_out: 10,
            rows_scanned: 2_000,
            ..clean()
        };

        assert_eq!(check(&m, &Budget::HOT, &rows(&[]))[0].rule, "amplification");
    }

    /// A batch statement does work per value it was handed, however few rows
    /// come back.
    #[test]
    fn a_batch_is_measured_against_what_it_was_handed() {
        let m = Measured {
            rows_out: 1,
            rows_scanned: 640,
            inputs: 64,
            ..clean()
        };

        assert!(check(&m, &Budget::HOT, &rows(&[])).is_empty());
    }

    /// A sweep that finds no work is the healthy steady state, and it returns
    /// no rows to divide by.
    #[test]
    fn a_query_returning_nothing_has_no_ratio() {
        let m = Measured {
            rows_out: 0,
            rows_scanned: 500,
            ..clean()
        };

        assert!(check(&m, &Budget::HOT, &rows(&[])).is_empty());
    }

    #[test]
    fn an_aggregate_reads_many_rows_to_return_one_by_design() {
        let m = Measured {
            rows_out: 1,
            rows_scanned: 500_000,
            worst_filtered: ("cached_path".to_string(), 400_000),
            collapses: true,
            ..clean()
        };

        assert!(check(&m, &Budget::HOT, &rows(&[])).is_empty());
    }

    #[test]
    fn spill_is_a_warning_under_sweep_and_fatal_under_hot() {
        let m = Measured {
            spilled: true,
            ..clean()
        };

        assert!(check(&m, &Budget::HOT, &rows(&[]))[0].fatal);
        assert!(!check(&m, &Budget::SWEEP, &rows(&[]))[0].fatal);
    }

    /// The `OFFSET 0` fence makes the recursive term a nested loop; losing it is
    /// the walk collapsing into a join over the whole edge table.
    #[test]
    fn walk_requires_the_fence_in_the_recursive_term() {
        let m = Measured {
            fenced_types: vec!["Merge Join".into()],
            ..clean()
        };

        let v = check(&m, &Budget::WALK, &rows(&[]));
        assert_eq!(v.len(), 1, "{v:?}");
        assert_eq!(v[0].rule, "shape_required");
        assert!(matches!(
            Budget::WALK.shape[0],
            Shape::Require("Nested Loop")
        ));
    }

    #[test]
    fn a_forbidden_node_in_the_recursive_term_is_a_violation() {
        let budget = Budget {
            shape: &[Shape::Forbid("Merge Join")],
            ..Budget::WALK
        };
        let m = Measured {
            fenced_types: vec!["Merge Join".into()],
            ..clean()
        };

        assert_eq!(check(&m, &budget, &rows(&[]))[0].rule, "shape_forbidden");
    }

    /// The fence the shape rules are about lives in a recursive term, so a plan
    /// that recurses nowhere has none to lose.
    #[test]
    fn a_plan_without_a_recursion_is_not_shape_checked() {
        let m = Measured {
            node_types: vec!["Merge Join".into()],
            ..clean()
        };

        assert!(check(&m, &Budget::WALK, &rows(&[])).is_empty());
    }

    #[test]
    fn an_override_far_under_budget_warns_so_it_gets_deleted() {
        let budget = Budget::hot().buffers(9_500).because("observed 8_412");
        let m = Measured {
            buffers: 100,
            ..clean()
        };

        let v = check(&m, &budget, &rows(&[]));
        assert_eq!(v[0].rule, "stale_override");
        assert!(!v[0].fatal);
    }
}
