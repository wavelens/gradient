/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Applies one query's budget to what its plan actually did. `relation_rows` is
//! `pg_class.reltuples` per relation, which is what makes a sequential scan of a
//! six-row lookup table legal and one of half a million rows a failure.

use std::collections::HashMap;

use super::{Budget, Measured, Shape, Spill, Violation};

pub fn check(
    measured: &Measured,
    budget: &Budget,
    relation_rows: &HashMap<String, u64>,
) -> Vec<Violation> {
    let mut out = Vec::new();

    if let Some(limit) = budget.seq_scan_rows {
        for relation in &measured.seq_scans {
            let size = relation_rows.get(relation).copied().unwrap_or_default();
            if size > limit {
                out.push(Violation {
                    rule: "seq_scan",
                    detail: format!("Seq Scan on {relation} ({size} rows, limit {limit})"),
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

    let amplification = measured.rows_scanned / measured.rows_out.max(1);
    if amplification > budget.amplification {
        out.push(Violation {
            rule: "amplification",
            detail: format!(
                "{} rows scanned for {} returned ({amplification}x, limit {}x)",
                measured.rows_scanned, measured.rows_out, budget.amplification,
            ),
            fatal: true,
        });
    }

    if measured.worst_filtered.1 > budget.rows_removed {
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

    for shape in budget.shape {
        match shape {
            Shape::Require(node) if !measured.node_types.iter().any(|n| n == node) => {
                out.push(Violation {
                    rule: "shape_required",
                    detail: format!("plan lost its {node}"),
                    fatal: true,
                });
            }

            Shape::Forbid(node) if measured.node_types.iter().any(|n| n == node) => {
                out.push(Violation {
                    rule: "shape_forbidden",
                    detail: format!("plan contains a {node}"),
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::check;
    use crate::sql::{Budget, Measured, Shape};

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
    fn a_big_table_seq_scan_is_fatal_on_hot() {
        let m = Measured {
            seq_scans: vec!["derivation".into()],
            ..clean()
        };

        let v = check(&m, &Budget::HOT, &rows(&[("derivation", 500_000)]));
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].rule, "seq_scan");
        assert!(v[0].fatal);
    }

    #[test]
    fn a_small_table_seq_scan_is_fine() {
        let m = Measured {
            seq_scans: vec!["role".into()],
            ..clean()
        };

        assert!(check(&m, &Budget::HOT, &rows(&[("role", 6)])).is_empty());
    }

    #[test]
    fn sweep_permits_any_seq_scan() {
        let m = Measured {
            seq_scans: vec!["derivation".into()],
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

    #[test]
    fn a_query_returning_nothing_still_has_an_amplification() {
        let m = Measured {
            rows_out: 0,
            rows_scanned: 500,
            ..clean()
        };

        let v = check(&m, &Budget::HOT, &rows(&[]));
        assert_eq!(
            v[0].rule, "amplification",
            "zero rows must not divide by zero"
        );
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

    #[test]
    fn walk_requires_a_nested_loop_and_forbids_a_merge_join() {
        let m = Measured {
            node_types: vec!["Merge Join".into()],
            ..clean()
        };

        let v = check(&m, &Budget::WALK, &rows(&[]));
        let rules: Vec<_> = v.iter().map(|x| x.rule).collect();
        assert!(rules.contains(&"shape_required"), "{rules:?}");
        assert!(rules.contains(&"shape_forbidden"), "{rules:?}");
        assert!(matches!(
            Budget::WALK.shape[0],
            Shape::Require("Nested Loop")
        ));
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
