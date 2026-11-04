/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::BTreeMap;

use chrono::NaiveDateTime;
use gradient_types::*;
use gradient_util::glob::{glob_match, is_pattern};
use gradient_wire::types::FlakeInputSourceWire;
use tracing::warn;

use crate::Scheduler;
use crate::jobs::{PendingEvalJob, PendingJob};

impl Scheduler {
    pub async fn persist_flake_input_sources(
        &self,
        job_id: &str,
        sources: Vec<FlakeInputSourceWire>,
    ) {
        let Some(PendingJob::Eval(PendingEvalJob {
            task_id: Some(task),
            ..
        })) = self.active_job(job_id).await
        else {
            return;
        };
        if let Err(e) = self.store_flake_input_sources(task, sources).await {
            warn!(error = %e, %task, "Failed to save repository for tracked flake input");
        }
    }

    async fn store_flake_input_sources(
        &self,
        task: TaskId,
        reported: Vec<FlakeInputSourceWire>,
    ) -> Result<(), sea_orm::DbErr> {
        let db = &self.db.worker_db;
        let tracked: Vec<String> = gradient_db::flake_input_overrides::of_task(db, task)
            .await?
            .into_iter()
            .map(|o| o.input_name)
            .collect();
        let stored = gradient_db::flake_input_sources::of_task(db, task).await?;
        let merged = merge_input_sources(stored, reported, &tracked, task, gradient_types::now());
        gradient_db::flake_input_sources::replace(db, task, merged).await
    }
}

/// The report can cover only the inputs of one evaluation. Saved rows for other tracked inputs
/// remain until removal of those inputs from tracking.
fn merge_input_sources(
    stored: Vec<MTaskFlakeInputSource>,
    reported: Vec<FlakeInputSourceWire>,
    tracked: &[String],
    task: TaskId,
    now: NaiveDateTime,
) -> Vec<MTaskFlakeInputSource> {
    let mut by_name: BTreeMap<String, MTaskFlakeInputSource> = stored
        .into_iter()
        .map(|row| (row.input_name.clone(), row))
        .collect();
    for source in reported {
        by_name.insert(
            source.name.clone(),
            MTaskFlakeInputSource {
                task,
                input_name: source.name,
                repository_url: source.repository_url,
                branch: source.branch,
                updated_at: now,
            },
        );
    }
    by_name
        .into_values()
        .filter(|row| is_tracked(&row.input_name, tracked))
        .collect()
}

fn is_tracked(name: &str, tracked: &[String]) -> bool {
    tracked
        .iter()
        .any(|t| t == name || (is_pattern(t) && glob_match(t, name)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wire(name: &str, url: &str) -> FlakeInputSourceWire {
        FlakeInputSourceWire {
            name: name.into(),
            repository_url: url.into(),
            branch: None,
        }
    }

    fn row(name: &str, url: &str) -> MTaskFlakeInputSource {
        MTaskFlakeInputSource {
            task: TaskId::nil(),
            input_name: name.into(),
            repository_url: url.into(),
            branch: None,
            updated_at: NaiveDateTime::default(),
        }
    }

    fn names_and_urls(rows: &[MTaskFlakeInputSource]) -> Vec<(&str, &str)> {
        rows.iter()
            .map(|r| (r.input_name.as_str(), r.repository_url.as_str()))
            .collect()
    }

    #[test]
    fn a_report_of_one_input_keeps_the_other_tracked_inputs() {
        let merged = merge_input_sources(
            vec![
                row("home-manager", "https://a/hm"),
                row("nixpkgs", "https://a/old"),
            ],
            vec![wire("nixpkgs", "https://a/new")],
            &["nixpkgs".into(), "home-*".into()],
            TaskId::nil(),
            NaiveDateTime::default(),
        );
        assert_eq!(
            names_and_urls(&merged),
            vec![
                ("home-manager", "https://a/hm"),
                ("nixpkgs", "https://a/new")
            ]
        );
    }

    #[test]
    fn rows_of_inputs_no_longer_tracked_are_dropped() {
        let merged = merge_input_sources(
            vec![row("old-input", "https://a/old")],
            vec![wire("nixpkgs", "https://a/nixpkgs")],
            &["nixpkgs".into()],
            TaskId::nil(),
            NaiveDateTime::default(),
        );
        assert_eq!(
            names_and_urls(&merged),
            vec![("nixpkgs", "https://a/nixpkgs")]
        );
    }
}
