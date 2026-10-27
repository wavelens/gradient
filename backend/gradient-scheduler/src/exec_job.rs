/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_db::scheduling::assignment_record::EXEC_KEY_PREFIX;
use gradient_pool::WorkerCaps;
use gradient_types::exec::{ExecRequirement, ExecSpec, Phase};
use gradient_types::ids::{EvaluationId, ExecJobAttemptId, ExecJobId, ProjectId, TaskId};
use gradient_wire::types::{ExecJob, RequiredPath};

#[derive(Debug, Clone)]
pub struct PendingExecJob {
    pub exec_job: ExecJobId,
    pub evaluation_id: EvaluationId,
    pub project_id: ProjectId,
    pub task_id: TaskId,
    pub entry_point: String,
    pub repository: String,
    pub commit: String,
    pub architecture: String,
    pub spec: ExecSpec,
    pub image_path: Option<String>,
    pub retries_left: u32,
    pub attempts_done: u32,
    pub last_phase: Option<Phase>,
    pub required_paths: Vec<RequiredPath>,
    pub queued_at: chrono::NaiveDateTime,
    pub ready_at: chrono::NaiveDateTime,
    pub rescore_count: u32,
    pub api_url: String,
    pub assignment: Option<ExecAssignment>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecAssignment {
    pub exec_job: ExecJobId,
    pub attempt: ExecJobAttemptId,
    pub number: u32,
    pub retries_cap: u32,
}

pub fn exec_job_key(job: ExecJobId) -> String {
    format!("{EXEC_KEY_PREFIX}{job}")
}

impl PendingExecJob {
    pub fn requirement_for(&self, caps: &WorkerCaps) -> Option<ExecRequirement> {
        let inventory = caps.exec.as_ref()?;
        Some(
            self.spec
                .requirement_on(&self.architecture, &inventory.defaults),
        )
    }

    pub fn fits(&self, caps: &WorkerCaps) -> bool {
        self.requirement_for(caps)
            .is_some_and(|requirement| caps.can_run_exec(&requirement))
    }

    pub fn assignment_on(&self, caps: &WorkerCaps) -> Option<ExecAssignment> {
        let inventory = caps.exec.as_ref()?;
        let limits = self.spec.limits_on(&inventory.allow, &inventory.defaults);
        Some(ExecAssignment {
            exec_job: self.exec_job,
            attempt: ExecJobAttemptId::now_v7(),
            number: self.attempts_done.saturating_add(1),
            retries_cap: limits.retries,
        })
    }

    pub fn exec_job_for(&self, caps: &WorkerCaps, assignment: &ExecAssignment) -> ExecJob {
        let inventory = caps.exec.clone().unwrap_or_default();
        let limits = self.spec.limits_on(&inventory.allow, &inventory.defaults);
        ExecJob {
            program: self.spec.program.clone(),
            args: self.spec.args.clone(),
            env: self.spec.env.clone(),
            image: self.image_path.clone(),
            requirement: self
                .spec
                .requirement_on(&self.architecture, &inventory.defaults),
            context: gradient_wire::types::JobContext {
                job: self.exec_job.to_string(),
                attempt: assignment.attempt.to_string(),
                evaluation: self.evaluation_id.to_string(),
                task: self.task_id.to_string(),
                project: self.project_id.to_string(),
                repository: self.repository.clone(),
                commit: self.commit.clone(),
                entry_point: self.entry_point.clone(),
                last_phase: self.last_phase.clone(),
                api_url: self.api_url.clone(),
                token: String::new(),
            },
            secrets: self.spec.secrets.clone(),
            keep_running: self.spec.keep_running,
            timeout_secs: limits.timeout_secs,
            max_silent_secs: limits.max_silent_secs,
            attempt: assignment.number,
        }
    }
}
