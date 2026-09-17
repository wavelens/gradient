/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! What the gate prints and what it exits with. The table is printed on success
//! too: the numbers are how a human calibrates the budgets the assertion cannot.

use gradient_db::sql::{Measured, Query, Violation};

pub enum Outcome {
    Pass(Measured),
    Fail(Vec<Violation>),
    /// A relation the plan touches is empty, or no value of a declared kind
    /// exists to bind. Measuring it would prove nothing, so it is reported
    /// rather than asserted on.
    Unmeasured(String),
}

pub fn render(rows: &[(&'static Query, Outcome)]) -> String {
    let measured = rows
        .iter()
        .filter(|(_, outcome)| !matches!(outcome, Outcome::Unmeasured(_)))
        .count();
    let unmeasured = rows.len() - measured;
    let over = rows
        .iter()
        .filter(|(_, outcome)| {
            matches!(outcome, Outcome::Fail(violations) if violations.iter().any(|v| v.fatal))
        })
        .count();

    let mut out = format!(
        "gradient-sql-gate: {} queries | {measured} measured | {unmeasured} unmeasured | {over} over budget\n\n",
        rows.len(),
    );

    for (query, outcome) in rows {
        match outcome {
            Outcome::Pass(m) => out.push_str(&format!(
                "PASS {:<24} {:<30} buffers {:>8} amp {:>6}x {:>8.1} ms\n",
                query.location(),
                query.name,
                m.buffers,
                m.rows_scanned / m.rows_out.max(1),
                m.execution_ms,
            )),

            Outcome::Fail(violations) => {
                out.push_str(&format!(
                    "FAIL {:<24} {:<30}\n",
                    query.location(),
                    query.name
                ));

                for violation in violations {
                    let tag = if violation.fatal { "     " } else { "warn " };
                    out.push_str(&format!(
                        "{tag}  {}: {}\n",
                        violation.rule, violation.detail
                    ));
                }

                out.push_str(&statement(query));
            }

            Outcome::Unmeasured(why) => {
                out.push_str(&format!(
                    "WARN {:<24} {:<30} unmeasured: {why}\n",
                    query.location(),
                    query.name,
                ));

                out.push_str(&statement(query));
            }
        }
    }

    out
}

/// The statement behind anything that is not a pass. A `file:line` names where
/// it was declared, which is not what the planner was given: a `sql_fn!` builds
/// its text at runtime and is not in that file at all.
fn statement(query: &Query) -> String {
    query
        .text()
        .lines()
        .map(|line| format!("       {}\n", line.trim_end()))
        .collect()
}

pub fn exit_code(rows: &[(&'static Query, Outcome)], max_unmeasured: usize) -> i32 {
    let unmeasured = rows
        .iter()
        .filter(|(_, outcome)| matches!(outcome, Outcome::Unmeasured(_)))
        .count();
    let fatal = rows.iter().any(|(_, outcome)| {
        matches!(outcome, Outcome::Fail(violations) if violations.iter().any(|v| v.fatal))
    });

    i32::from(fatal || unmeasured > max_unmeasured)
}

#[cfg(test)]
mod tests {
    use gradient_db::sql::{Budget, Measured, Param, Query, Sql, Tier, Violation};

    use super::{Outcome, exit_code, render};

    static Q: Query = Query {
        name: "SAMPLE",
        sql: Sql::Static("SELECT 1"),
        file: "src/readiness.rs",
        line: 348,
        params: &[Param::DerivationId],
        tier: Tier::Hot,
        budget: Budget::HOT,
        flags: &[],
    };

    #[test]
    fn a_violation_renders_with_its_location() {
        let rows = vec![(
            &Q,
            Outcome::Fail(vec![Violation {
                rule: "buffers",
                detail: "buffers 48213 > 2000".into(),
                fatal: true,
            }]),
        )];

        let text = render(&rows);
        assert!(text.contains("readiness.rs:348"), "{text}");
        assert!(text.contains("SAMPLE"), "{text}");
        assert!(text.contains("buffers 48213 > 2000"), "{text}");
        assert!(text.contains("SELECT 1"), "the statement itself: {text}");
    }

    #[test]
    fn unmeasured_says_why_and_does_not_fail() {
        let rows = vec![(&Q, Outcome::Unmeasured("debug_info has 0 rows".into()))];
        assert!(render(&rows).contains("debug_info has 0 rows"));
        assert!(render(&rows).contains("SELECT 1"));
        assert_eq!(exit_code(&rows, 1), 0);
    }

    #[test]
    fn growing_the_unmeasured_count_fails() {
        let rows = vec![(&Q, Outcome::Unmeasured("empty".into()))];
        assert_eq!(exit_code(&rows, 0), 1);
    }

    #[test]
    fn a_fatal_violation_fails_and_a_warning_does_not() {
        let fatal = vec![(
            &Q,
            Outcome::Fail(vec![Violation {
                rule: "buffers",
                detail: "over".into(),
                fatal: true,
            }]),
        )];
        let warn = vec![(
            &Q,
            Outcome::Fail(vec![Violation {
                rule: "stale_override",
                detail: "under".into(),
                fatal: false,
            }]),
        )];

        assert_eq!(exit_code(&fatal, 0), 1);
        assert_eq!(exit_code(&warn, 0), 0);
    }

    #[test]
    fn the_summary_counts_every_class() {
        let rows = vec![
            (&Q, Outcome::Pass(Measured::default())),
            (&Q, Outcome::Unmeasured("empty".into())),
        ];

        let text = render(&rows);
        assert!(text.contains("2 queries"), "{text}");
        assert!(text.contains("1 measured"), "{text}");
        assert!(text.contains("1 unmeasured"), "{text}");
    }
}
