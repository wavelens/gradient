/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use serde_json::Value;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Measured {
    pub buffers: u64,
    pub rows_out: u64,
    pub rows_scanned: u64,
    pub max_loops: u64,
    pub worst_filtered: (String, u64),
    pub spilled: bool,
    pub seq_scans: Vec<Scan>,
    pub node_types: Vec<String>,
    pub fenced_types: Vec<String>,
    pub collapses: bool,
    pub inputs: u64,
    pub execution_ms: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Scan {
    pub relation: String,
    pub read: u64,
    pub removed: u64,
}

#[derive(Debug)]
pub struct PlanError(String);

impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for PlanError {}

pub fn measure(body: &Value) -> Result<Measured, PlanError> {
    let entry = body
        .get(0)
        .ok_or_else(|| PlanError("EXPLAIN body is empty".into()))?;
    let root = entry
        .get("Plan")
        .ok_or_else(|| PlanError("EXPLAIN body has no Plan".into()))?;

    let mut measured = Measured {
        buffers: number(root, "Shared Hit Blocks") + number(root, "Shared Read Blocks"),
        rows_out: rows_out(root),
        collapses: root.get("Node Type").and_then(Value::as_str) == Some("Result"),
        execution_ms: entry
            .get("Execution Time")
            .and_then(Value::as_f64)
            .unwrap_or_default(),
        ..Default::default()
    };

    walk(root, &mut measured);
    fenced(root, &mut measured);
    Ok(measured)
}

/// A write without `RETURNING` is reporting no rows at the root.
/// Its modified rows are what its child fed into the `ModifyTable`.
fn rows_out(root: &Value) -> u64 {
    let rows = number(root, "Actual Rows");
    if rows > 0 || root.get("Node Type").and_then(Value::as_str) != Some("ModifyTable") {
        return rows;
    }

    root.get("Plans")
        .and_then(Value::as_array)
        .map(|children| {
            children
                .iter()
                .map(|child| number(child, "Actual Rows") * number(child, "Actual Loops").max(1))
                .sum()
        })
        .unwrap_or_default()
}

fn walk(node: &Value, measured: &mut Measured) {
    let loops = number(node, "Actual Loops").max(1);
    let rows = number(node, "Actual Rows") * loops;
    measured.rows_scanned += rows;
    measured.max_loops = measured.max_loops.max(loops);

    if let Some(kind) = node.get("Node Type").and_then(Value::as_str) {
        measured.node_types.push(kind.to_string());
        measured.collapses |= kind == "Aggregate";

        if kind == "Seq Scan"
            && let Some(relation) = node.get("Relation Name").and_then(Value::as_str)
        {
            measured.seq_scans.push(Scan {
                relation: relation.to_string(),
                read: rows + number(node, "Rows Removed by Filter") * loops,
                removed: number(node, "Rows Removed by Filter") * loops,
            });
        }
    }

    let filtered = number(node, "Rows Removed by Filter") * loops;
    if filtered > measured.worst_filtered.1 {
        let who = node
            .get("Relation Name")
            .or_else(|| node.get("Node Type"))
            .and_then(Value::as_str)
            .unwrap_or("?");
        measured.worst_filtered = (who.to_string(), filtered);
    }

    measured.spilled |= node.get("Sort Space Type").and_then(Value::as_str) == Some("Disk")
        || number(node, "Hash Batches") > 1
        || number(node, "Temp Read Blocks") > 0
        || number(node, "Temp Written Blocks") > 0;

    if let Some(children) = node.get("Plans").and_then(Value::as_array) {
        for child in children {
            walk(child, measured);
        }
    }
}

fn fenced(node: &Value, measured: &mut Measured) {
    let children = node.get("Plans").and_then(Value::as_array);

    if node.get("Node Type").and_then(Value::as_str) == Some("Recursive Union")
        && let Some(term) = children.and_then(|plans| plans.get(1))
    {
        collect_types(term, &mut measured.fenced_types);
    }

    for child in children.into_iter().flatten() {
        fenced(child, measured);
    }
}

fn collect_types(node: &Value, out: &mut Vec<String>) {
    if let Some(kind) = node.get("Node Type").and_then(Value::as_str) {
        out.push(kind.to_string());
    }

    for child in node
        .get("Plans")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        collect_types(child, out);
    }
}

/// Postgres 18 is reporting `Actual Rows` as a float.
fn number(node: &Value, key: &str) -> u64 {
    node.get(key)
        .and_then(Value::as_f64)
        .unwrap_or_default()
        .round() as u64
}

#[cfg(test)]
mod tests {
    use super::measure;

    fn fixture(name: &str) -> serde_json::Value {
        let body = std::fs::read_to_string(format!(
            "{}/tests/fixtures/{name}.json",
            env!("CARGO_MANIFEST_DIR")
        ))
        .expect("fixture must exist");

        serde_json::from_str(&body).expect("fixture must be JSON")
    }

    #[test]
    fn root_buffers_are_the_total() {
        let m = measure(&fixture("seq_scan_hot")).expect("measurable");
        assert_eq!(m.buffers, 48_213);
        assert_eq!(m.rows_out, 1);
    }

    #[test]
    fn seq_scans_are_named_with_what_they_threw_away() {
        let m = measure(&fixture("seq_scan_hot")).expect("measurable");
        assert_eq!(m.seq_scans.len(), 1);
        assert_eq!(m.seq_scans[0].relation, "derivation_build");
        assert_eq!(m.seq_scans[0].removed, 412_816);
        assert_eq!(m.seq_scans[0].read, 825_696);
        assert_eq!(m.worst_filtered, ("derivation_build".to_string(), 412_816));
    }

    #[test]
    fn a_recursive_term_is_the_second_child_of_its_union() {
        let body = serde_json::json!([{
            "Plan": {
                "Node Type": "Recursive Union",
                "Actual Rows": 3.0,
                "Actual Loops": 1,
                "Plans": [
                    { "Node Type": "Merge Join", "Actual Rows": 1.0, "Actual Loops": 1 },
                    { "Node Type": "Nested Loop", "Actual Rows": 2.0, "Actual Loops": 1 },
                ],
            }
        }]);

        let m = measure(&body).expect("measurable");
        assert_eq!(m.fenced_types, vec!["Nested Loop".to_string()]);
        assert!(
            m.node_types.iter().any(|n| n == "Merge Join"),
            "the seed is still measured"
        );
    }

    #[test]
    fn an_aggregate_collapses_what_it_read() {
        let body = serde_json::json!([{
            "Plan": {
                "Node Type": "Aggregate",
                "Actual Rows": 1.0,
                "Actual Loops": 1,
                "Plans": [{ "Node Type": "Seq Scan", "Actual Rows": 500.0, "Actual Loops": 1 }],
            }
        }]);

        assert!(measure(&body).expect("measurable").collapses);
    }

    #[test]
    fn a_scalar_from_a_subplan_collapses_too() {
        let body = serde_json::json!([{
            "Plan": { "Node Type": "Result", "Actual Rows": 1.0, "Actual Loops": 1 }
        }]);

        assert!(measure(&body).expect("measurable").collapses);
    }

    #[test]
    fn rows_scanned_multiplies_by_loops() {
        let m = measure(&fixture("nested_loop_walk")).expect("measurable");
        assert_eq!(m.rows_scanned, 132_001);
        assert_eq!(m.max_loops, 44_000);
        assert!(m.node_types.iter().any(|n| n == "Nested Loop"));
    }

    #[test]
    fn disk_sort_is_a_spill() {
        assert!(
            measure(&fixture("spilled_sort"))
                .expect("measurable")
                .spilled
        );
    }

    #[test]
    fn a_write_returns_what_it_modified() {
        let body = serde_json::json!([{
            "Plan": {
                "Node Type": "ModifyTable",
                "Operation": "Update",
                "Actual Rows": 0.0,
                "Actual Loops": 1,
                "Shared Hit Blocks": 42,
                "Plans": [{ "Node Type": "Index Scan", "Actual Rows": 8.0, "Actual Loops": 1 }],
            }
        }]);

        let m = measure(&body).expect("measurable");
        assert_eq!(m.rows_out, 8, "an UPDATE without RETURNING reports no rows");
        assert_eq!(m.rows_scanned, 8);
    }

    #[test]
    fn a_read_that_returns_nothing_still_returns_nothing() {
        let body = serde_json::json!([{
            "Plan": { "Node Type": "Seq Scan", "Actual Rows": 0.0, "Actual Loops": 1 }
        }]);

        assert_eq!(measure(&body).expect("measurable").rows_out, 0);
    }

    #[test]
    fn a_body_without_a_plan_is_an_error() {
        let err = measure(&serde_json::json!([{}])).expect_err("no Plan key");
        assert!(format!("{err}").contains("Plan"));
    }
}
