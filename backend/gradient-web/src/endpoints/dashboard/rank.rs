/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::EvaluationId;
use serde::{Deserialize, Serialize};
use std::cmp::Reverse;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    StarredActive = 1,
    Active = 2,
    Starred = 3,
    Member = 4,
}

impl Tier {
    pub fn of(starred: bool, active: bool) -> Self {
        match (starred, active) {
            (true, true) => Tier::StarredActive,
            (false, true) => Tier::Active,
            (true, false) => Tier::Starred,
            (false, false) => Tier::Member,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Filter {
    #[default]
    All,
    Failing,
    Worse,
    Starred,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Outcomes {
    pub ok: i64,
    pub failing: i64,
    pub total: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Latest {
    pub id: EvaluationId,
    pub status: EvaluationStatus,
    pub commit: String,
    pub created_at: NaiveDateTime,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TaskFacts {
    pub project: String,
    pub task: String,
    pub starred: bool,
    pub latest: Option<Latest>,
    pub previous: Option<EvaluationId>,
    pub recent_14d: i64,
    pub last_28d: i64,
    pub completed_30d: i64,
    pub failed_30d: i64,
    pub speed_ms: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct HistoryBar {
    pub id: EvaluationId,
    pub status: EvaluationStatus,
    pub duration_ms: Option<i64>,
    pub created_at: NaiveDateTime,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TaskRow {
    pub project: String,
    pub task: String,
    pub starred: bool,
    pub tier: Tier,
    pub latest: Option<Latest>,
    pub entry_points: Option<Outcomes>,
    pub delta: Option<i64>,
    pub speed_ms: Option<i64>,
    pub reliability: Option<f64>,
    pub evaluations_per_week: f64,
    pub history: Vec<HistoryBar>,
    #[serde(skip)]
    failing: bool,
}

impl TaskRow {
    pub fn build(f: TaskFacts, outcomes: &HashMap<EvaluationId, Outcomes>) -> Self {
        let latest_outcomes = f.latest.as_ref().and_then(|l| outcomes.get(&l.id)).copied();
        let delta = f
            .latest
            .as_ref()
            .filter(|l| EvaluationStatus::TERMINAL.contains(&l.status))
            .and(latest_outcomes)
            .zip(f.previous.and_then(|p| outcomes.get(&p)))
            .map(|(l, p)| l.failing - p.failing);
        let failing = f
            .latest
            .as_ref()
            .is_some_and(|l| l.status == EvaluationStatus::Failed)
            || latest_outcomes.is_some_and(|o| o.failing > 0);
        let judged = f.completed_30d + f.failed_30d;
        TaskRow {
            tier: Tier::of(f.starred, f.recent_14d > 0),
            reliability: (judged > 0).then(|| f.completed_30d as f64 / judged as f64),
            evaluations_per_week: f.last_28d as f64 / 4.0,
            entry_points: latest_outcomes,
            delta,
            failing,
            history: Vec::new(),
            project: f.project,
            task: f.task,
            starred: f.starred,
            latest: f.latest,
            speed_ms: f.speed_ms,
        }
    }

    pub fn matches(&self, filter: Filter) -> bool {
        match filter {
            Filter::All => true,
            Filter::Failing => self.failing,
            Filter::Worse => self.delta.is_some_and(|d| d > 0),
            Filter::Starred => self.starred,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Counts {
    pub all: usize,
    pub failing: usize,
    pub worse: usize,
    pub starred: usize,
}

pub fn counts(rows: &[TaskRow]) -> Counts {
    let n = |f| rows.iter().filter(|r| r.matches(f)).count();
    Counts {
        all: rows.len(),
        failing: n(Filter::Failing),
        worse: n(Filter::Worse),
        starred: n(Filter::Starred),
    }
}

pub fn rank(rows: &mut [TaskRow]) {
    rows.sort_by(|a, b| {
        a.tier
            .cmp(&b.tier)
            .then_with(|| {
                Reverse(a.latest.as_ref().map(|l| l.created_at))
                    .cmp(&Reverse(b.latest.as_ref().map(|l| l.created_at)))
            })
            .then_with(|| (&a.project, &a.task).cmp(&(&b.project, &b.task)))
    });
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Paging {
    pub offset: usize,
    pub limit: usize,
}

impl Paging {
    pub fn from_query(page: Option<u64>, per_page: Option<u64>) -> Self {
        let limit = per_page.unwrap_or(10).clamp(1, 25) as usize;
        let page = page.unwrap_or(1).max(1) as usize;
        Paging {
            offset: (page - 1) * limit,
            limit,
        }
    }
}

pub fn history_len(requested: Option<u64>) -> u64 {
    requested.unwrap_or(30).clamp(1, 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn at(day: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 9, day)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap()
    }

    fn facts(
        task: &str,
        starred: bool,
        recent: i64,
        latest: Option<(EvaluationStatus, u32)>,
    ) -> TaskFacts {
        TaskFacts {
            project: "p".into(),
            task: task.into(),
            starred,
            latest: latest.map(|(status, day)| Latest {
                id: EvaluationId::now_v7(),
                status,
                commit: "a1b2c3d".into(),
                created_at: at(day),
            }),
            previous: Some(EvaluationId::now_v7()),
            recent_14d: recent,
            last_28d: 8,
            completed_30d: 9,
            failed_30d: 1,
            speed_ms: Some(60_000),
        }
    }

    fn outcomes(
        row: &TaskFacts,
        l_failing: i64,
        p_failing: i64,
    ) -> HashMap<EvaluationId, Outcomes> {
        let mut m = HashMap::new();
        if let Some(l) = &row.latest {
            m.insert(
                l.id,
                Outcomes {
                    ok: 10 - l_failing,
                    failing: l_failing,
                    total: 10,
                },
            );
        }
        if let Some(p) = row.previous {
            m.insert(
                p,
                Outcomes {
                    ok: 10 - p_failing,
                    failing: p_failing,
                    total: 10,
                },
            );
        }
        m
    }

    #[test]
    fn tiers_follow_star_then_activity() {
        assert_eq!(Tier::of(true, true), Tier::StarredActive);
        assert_eq!(Tier::of(false, true), Tier::Active);
        assert_eq!(Tier::of(true, false), Tier::Starred);
        assert_eq!(Tier::of(false, false), Tier::Member);
    }

    #[test]
    fn delta_is_failing_growth_once_latest_finished() {
        let f = facts("a", false, 1, Some((EvaluationStatus::Completed, 20)));
        let o = outcomes(&f, 3, 1);
        assert_eq!(TaskRow::build(f, &o).delta, Some(2));
    }

    #[test]
    fn delta_waits_for_a_running_latest() {
        let f = facts("a", false, 1, Some((EvaluationStatus::Building, 20)));
        let o = outcomes(&f, 3, 1);
        assert_eq!(TaskRow::build(f, &o).delta, None);
    }

    #[test]
    fn delta_needs_a_previous() {
        let mut f = facts("a", false, 1, Some((EvaluationStatus::Completed, 20)));
        let o = outcomes(&f, 3, 1);
        f.previous = None;
        assert_eq!(TaskRow::build(f, &o).delta, None);
    }

    #[test]
    fn row_without_evaluations() {
        let row = TaskRow::build(facts("a", false, 0, None), &HashMap::new());
        assert!(row.latest.is_none());
        assert_eq!(row.delta, None);
        assert_eq!(row.tier, Tier::Member);
        assert!(!row.matches(Filter::Failing));
    }

    #[test]
    fn failing_means_failed_or_failing_entry_points() {
        let f = facts("a", false, 1, Some((EvaluationStatus::Failed, 20)));
        assert!(TaskRow::build(f, &HashMap::new()).matches(Filter::Failing));
        let f = facts("b", false, 1, Some((EvaluationStatus::Completed, 20)));
        let o = outcomes(&f, 1, 1);
        assert!(TaskRow::build(f, &o).matches(Filter::Failing));
    }

    #[test]
    fn worse_means_positive_delta() {
        let f = facts("a", false, 1, Some((EvaluationStatus::Completed, 20)));
        let o = outcomes(&f, 1, 3);
        assert!(!TaskRow::build(f, &o).matches(Filter::Worse));
    }

    #[test]
    fn rank_orders_tier_then_newest() {
        let mut rows: Vec<TaskRow> = vec![
            facts("member", false, 0, Some((EvaluationStatus::Completed, 21))),
            facts(
                "active_old",
                false,
                2,
                Some((EvaluationStatus::Completed, 10)),
            ),
            facts("starred", true, 0, Some((EvaluationStatus::Completed, 1))),
            facts(
                "active_new",
                false,
                2,
                Some((EvaluationStatus::Completed, 22)),
            ),
            facts("both", true, 1, Some((EvaluationStatus::Completed, 2))),
        ]
        .into_iter()
        .map(|f| TaskRow::build(f, &HashMap::new()))
        .collect();
        rank(&mut rows);
        let names: Vec<&str> = rows.iter().map(|r| r.task.as_str()).collect();
        assert_eq!(
            names,
            ["both", "active_new", "active_old", "starred", "member"]
        );
    }

    #[test]
    fn counts_cover_every_row() {
        let rows: Vec<TaskRow> = vec![
            facts("a", true, 1, Some((EvaluationStatus::Failed, 20))),
            facts("b", false, 1, Some((EvaluationStatus::Completed, 20))),
        ]
        .into_iter()
        .map(|f| TaskRow::build(f, &HashMap::new()))
        .collect();
        assert_eq!(
            counts(&rows),
            Counts {
                all: 2,
                failing: 1,
                worse: 0,
                starred: 1
            }
        );
    }

    #[test]
    fn paging_and_history_are_clamped() {
        assert_eq!(
            Paging::from_query(None, None),
            Paging {
                offset: 0,
                limit: 10
            }
        );
        assert_eq!(
            Paging::from_query(Some(0), Some(1000)),
            Paging {
                offset: 0,
                limit: 25
            }
        );
        assert_eq!(
            Paging::from_query(Some(3), Some(25)),
            Paging {
                offset: 50,
                limit: 25
            }
        );
        assert_eq!(history_len(None), 30);
        assert_eq!(history_len(Some(0)), 1);
        assert_eq!(history_len(Some(999)), 60);
    }

    #[test]
    fn reliability_ignores_empty_windows() {
        let mut f = facts("a", false, 1, None);
        f.completed_30d = 0;
        f.failed_30d = 0;
        assert_eq!(TaskRow::build(f, &HashMap::new()).reliability, None);
    }
}
