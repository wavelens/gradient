/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::{StatusWrite, registry};

const TABLES: [&str; 3] = ["derivation_build", "evaluation", "exec_job"];
const CLAUSE_ENDS: [&str; 3] = ["WHERE", "FROM", "RETURNING"];

pub fn violations() -> Vec<String> {
    registry()
        .filter_map(|query| violation(query.name, &query.text(), query.status_write))
        .collect()
}

fn violation(name: &str, sql: &str, declared: Option<StatusWrite>) -> Option<String> {
    let written = written_statuses(sql);
    match (written.as_slice(), declared) {
        ([], None) => None,
        ([], Some(_)) => Some(format!(
            "{name} declares a status write and writes no status"
        )),
        (_, None) => Some(format!("{name} writes a status without `status_write`")),
        (written, Some(declared)) => written
            .iter()
            .find(|write| **write != Written::Literal(declared.table, declared.to))
            .map(|write| format!("{name} declares {declared:?} and writes {write:?}")),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Written {
    Literal(&'static str, i32),
    Computed(&'static str),
}

fn written_statuses(sql: &str) -> Vec<Written> {
    let words: Vec<&str> = sql.split_whitespace().collect();
    let mut written = Vec::new();
    for (index, pair) in words.windows(2).enumerate() {
        let table = pair[1].trim_start_matches("public.");
        let Some(table) = TABLES.into_iter().find(|known| *known == table) else {
            continue;
        };
        if !pair[0].eq_ignore_ascii_case("UPDATE") {
            continue;
        }
        let assignments = set_clause(&words[index + 2..]);
        written.extend(status_values(&assignments).map(|value| match value {
            Some(status) => Written::Literal(table, status),
            None => Written::Computed(table),
        }));
    }
    written
}

fn set_clause(words: &[&str]) -> String {
    let mut depth = 0i32;
    let mut clause = Vec::new();
    let after_set = words
        .iter()
        .skip_while(|word| !word.eq_ignore_ascii_case("SET"))
        .skip(1);
    for word in after_set {
        if depth == 0 && CLAUSE_ENDS.iter().any(|end| word.eq_ignore_ascii_case(end)) {
            break;
        }
        depth += word
            .chars()
            .map(|char| i32::from(char == '(') - i32::from(char == ')'))
            .sum::<i32>();
        clause.push(*word);
    }
    clause.join(" ")
}

fn status_values(assignments: &str) -> impl Iterator<Item = Option<i32>> {
    let mut depth = 0i32;
    let mut values = Vec::new();
    for assignment in assignments.split(|char| {
        depth += i32::from(char == '(') - i32::from(char == ')');
        char == ',' && depth == 0
    }) {
        let Some((column, value)) = assignment.split_once('=') else {
            continue;
        };
        if column.trim() != "status" {
            continue;
        }
        values.extend(assigned_literals(value.trim()));
    }
    values.into_iter()
}

fn assigned_literals(value: &str) -> Vec<Option<i32>> {
    if let Ok(status) = value.parse() {
        return vec![Some(status)];
    }
    if !value.to_ascii_uppercase().starts_with("CASE") {
        return vec![None];
    }
    let words: Vec<&str> = value.split_whitespace().collect();
    words
        .windows(2)
        .filter(|pair| pair[0].eq_ignore_ascii_case("THEN") || pair[0].eq_ignore_ascii_case("ELSE"))
        .filter(|pair| !pair[1].ends_with("status"))
        .map(|pair| pair[1].parse().ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const QUEUE: StatusWrite = StatusWrite {
        table: "derivation_build",
        to: 1,
    };

    #[test]
    fn every_status_write_declares_its_transition() {
        let violations = violations();
        assert!(violations.is_empty(), "{}", violations.join("\n"));
    }

    #[test]
    fn declared_literal_case_and_other_table_writes_are_allowed() {
        let queue = "UPDATE derivation_build db SET status = 1, updated_at = now() WHERE db.status = 0 RETURNING db.derivation";
        assert_eq!(violation("QUEUE", queue, Some(QUEUE)), None);

        let case = "UPDATE derivation_build d SET blocking_deps = d.blocking_deps + c.n, status = CASE WHEN d.status = 0 AND coalesce(d.attempt, 0) = 0 THEN 1 ELSE d.status END FROM (SELECT 1) c WHERE d.derivation = c.derivation";
        assert_eq!(violation("QUEUE", case, Some(QUEUE)), None);

        let bump = "UPDATE derivation_build SET attempt = attempt + 1 WHERE status = 1";
        assert_eq!(violation("BUMP", bump, None), None);
        let other_table = "UPDATE build_job SET status = 2 WHERE id = $1";
        assert_eq!(violation("JOB", other_table, None), None);
    }

    #[test]
    fn undeclared_wrong_or_bound_status_writes_are_flagged() {
        let undeclared = "UPDATE evaluation e SET status = 3 WHERE e.id = $1";
        assert!(violation("REOPEN", undeclared, None).is_some());
        let undeclared_exec = "UPDATE exec_job r SET status = 4 WHERE r.id = $1";
        assert!(violation("FAIL", undeclared_exec, None).is_some());

        let wrong = "UPDATE derivation_build SET status = 0 WHERE status = 1";
        assert!(violation("UNQUEUE", wrong, Some(QUEUE)).is_some());

        let bound =
            "UPDATE derivation_build SET cache_available = false, status = $2 WHERE id = $1";
        assert!(violation("CLEAR", bound, Some(QUEUE)).is_some());
    }
}
