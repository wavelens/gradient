/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_ci::ApplyInput;
use gradient_db::WorkerDb;
use gradient_entity::ids::DispatchedJobId;
use gradient_entity::ids::TaskTriggerId;
use gradient_entity::task_trigger::TriggerType;
use gradient_wire::types::{CandidateScore, ClusterMembership, EvalMessageLevel, EvalStatsReport};

use crate::Scheduler;

pub(super) fn candidate_score(
    key: &str,
    missing: u32,
    missing_nar_size: u64,
    outputs_present: bool,
) -> CandidateScore {
    CandidateScore {
        job_id: key.to_owned(),
        missing_count: missing,
        missing_nar_size,
        outputs_present,
    }
}

pub(super) async fn store_scores(scheduler: &Scheduler, worker: &str, scores: Vec<CandidateScore>) {
    scheduler.record_scores(worker, scores).await;
}

pub(super) async fn store_eval_stats(
    scheduler: &Scheduler,
    key: &str,
    stats: EvalStatsReport,
) -> anyhow::Result<()> {
    scheduler.record_eval_metrics(key, stats).await
}

pub(super) async fn store_eval_message(
    scheduler: &Scheduler,
    key: &str,
    level: EvalMessageLevel,
    source: String,
    message: String,
) -> anyhow::Result<()> {
    scheduler
        .record_eval_message(key, level, source, message)
        .await
}

pub(super) fn prepare_secs(membership: &ClusterMembership) -> u32 {
    membership.hold_secs
}

pub(super) async fn abandon_row(db: &WorkerDb, assignment: DispatchedJobId) {
    gradient_db::scheduling::assignment_record::abandon_open_assignment(db, assignment)
        .await
        .expect("the dispatched_job row closes");
}

pub(super) fn trigger_input(
    trigger: TaskTriggerId,
    trigger_type: TriggerType,
    branch: &str,
    commit: &[u8],
    instance_max_storage_gb: u32,
) -> ApplyInput {
    ApplyInput {
        trigger_id: trigger,
        trigger_type,
        branch: branch.to_owned(),
        commit_hash: commit.to_vec(),
        commit_message: None,
        author_name: None,
        manual: false,
        gate_approval: None,
        repository_override: None,
        wildcard_override: None,
        source_comment: None,
        instance_max_storage_gb,
    }
}
