/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::NaiveDateTime;
use gradient_types::{EvalCapability, ExecJobId, WaitingReason};
use gradient_util::sync::Mutex;

use crate::unbuildable::UNBUILDABLE_GRACE_SECS;

#[derive(Debug, Default, Clone)]
pub(crate) struct UnplaceableExecJobs(Arc<Mutex<HashMap<ExecJobId, Unplaceable>>>);

#[derive(Debug)]
struct Unplaceable {
    missing: String,
    since: NaiveDateTime,
}

impl UnplaceableExecJobs {
    pub(crate) fn overdue(
        &self,
        job: ExecJobId,
        reason: Option<&WaitingReason>,
        waits: bool,
        now: NaiveDateTime,
    ) -> Option<String> {
        let mut jobs = self.0.lock();
        let Some(missing) = reason.and_then(missing_workers) else {
            jobs.remove(&job);
            return None;
        };
        let entry = jobs.entry(job).or_insert_with(|| Unplaceable {
            missing: missing.clone(),
            since: now,
        });
        if entry.missing != missing {
            *entry = Unplaceable {
                missing,
                since: now,
            };
        }

        (!waits && (now - entry.since).num_seconds() >= UNBUILDABLE_GRACE_SECS).then(|| {
            format!(
                "{}. Enable waiting for workers on the task to wait instead.",
                entry.missing
            )
        })
    }

    pub(crate) fn retain(&self, queued: &HashSet<ExecJobId>) {
        self.0.lock().retain(|job, _| queued.contains(job));
    }

    #[cfg(test)]
    pub(crate) fn backdate(&self, by: chrono::Duration) {
        for entry in self.0.lock().values_mut() {
            entry.since -= by;
        }
    }
}

fn missing_workers(reason: &WaitingReason) -> Option<String> {
    match reason {
        WaitingReason::EvalWorkers {
            capability: EvalCapability::Exec,
            ..
        } => Some("No connected worker runs exec jobs".to_owned()),
        WaitingReason::ExecWorkers { unmet, .. } if !unmet.is_empty() => {
            Some(format!("No connected worker offers {unmet}"))
        }
        WaitingReason::ExecWorkers {
            matching_workers: 0,
            ..
        } => Some("No connected worker offers every requirement at once".to_owned()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use gradient_types::ExecUnmet;

    fn waiting(unmet: ExecUnmet, matching_workers: u32) -> WaitingReason {
        WaitingReason::ExecWorkers {
            unmet,
            connected_workers: 2,
            matching_workers,
        }
    }

    fn ssh() -> WaitingReason {
        waiting(
            ExecUnmet {
                ssh: true,
                ..ExecUnmet::default()
            },
            0,
        )
    }

    fn after(secs: i64) -> NaiveDateTime {
        NaiveDateTime::default() + Duration::seconds(secs)
    }

    #[test]
    fn a_job_no_worker_can_run_is_overdue_after_the_grace() {
        let jobs = UnplaceableExecJobs::default();
        let job = ExecJobId::now_v7();
        assert_eq!(jobs.overdue(job, Some(&ssh()), false, after(0)), None);
        assert_eq!(
            jobs.overdue(job, Some(&ssh()), false, after(UNBUILDABLE_GRACE_SECS)),
            Some(
                "No connected worker offers ssh. Enable waiting for workers on the task to wait instead."
                    .to_owned()
            )
        );
    }

    #[test]
    fn a_task_waiting_for_workers_keeps_the_job() {
        let jobs = UnplaceableExecJobs::default();
        let job = ExecJobId::now_v7();
        jobs.overdue(job, Some(&ssh()), true, after(0));
        assert_eq!(
            jobs.overdue(job, Some(&ssh()), true, after(UNBUILDABLE_GRACE_SECS * 10)),
            None
        );
    }

    #[test]
    fn busy_matching_workers_never_end_the_job() {
        let jobs = UnplaceableExecJobs::default();
        let job = ExecJobId::now_v7();
        let busy = waiting(ExecUnmet::default(), 1);
        jobs.overdue(job, Some(&busy), false, after(0));
        assert_eq!(
            jobs.overdue(job, Some(&busy), false, after(UNBUILDABLE_GRACE_SECS * 10)),
            None
        );
    }

    #[test]
    fn a_changed_or_cleared_gap_restarts_the_grace() {
        let jobs = UnplaceableExecJobs::default();
        let job = ExecJobId::now_v7();
        let no_exec_worker = WaitingReason::eval_workers(EvalCapability::Exec, 0);
        jobs.overdue(job, Some(&no_exec_worker), false, after(0));
        assert_eq!(
            jobs.overdue(job, Some(&ssh()), false, after(UNBUILDABLE_GRACE_SECS)),
            None,
            "the missing permission changed"
        );
        jobs.overdue(job, None, false, after(UNBUILDABLE_GRACE_SECS + 1));
        assert_eq!(
            jobs.overdue(job, Some(&ssh()), false, after(UNBUILDABLE_GRACE_SECS * 2)),
            None,
            "a worker fitted in between"
        );
    }

    #[test]
    fn workers_that_fit_only_apart_still_count_as_missing() {
        let jobs = UnplaceableExecJobs::default();
        let job = ExecJobId::now_v7();
        let apart = waiting(ExecUnmet::default(), 0);
        jobs.overdue(job, Some(&apart), false, after(0));
        assert!(
            jobs.overdue(job, Some(&apart), false, after(UNBUILDABLE_GRACE_SECS))
                .is_some()
        );
    }
}
