/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use gradient_entity::build_attempt::AttemptOutcome;
use gradient_entity::cache_upstream::CacheUpstreamKind;
use gradient_entity::cluster_attempt::ClusterAttemptOutcome;
use gradient_entity::cluster_job::ClusterJobStatus;
use gradient_entity::dispatched_job::{DispatchedJobKind, DispatchedJobOutcome};
use gradient_entity::evaluation::EvaluationKind;
use gradient_entity::ids::{
    BuildAttemptId, CacheUpstreamId, ClusterAttemptId, ClusterJobId, ClusterMemberId, CommitId,
    DerivationFeatureId, DerivationMetricId, DispatchedJobId, DispatchedJobPhaseId,
    EvalCacheStoreId, EvaluationFlakeInputOverrideId, EvaluationId, EvaluationInputUpdateId,
    EvaluationMetricId, FeatureId, FlakeInputOverrideId, IntegrationId, ProjectId, TaskActionId,
    TaskId, TaskTriggerId,
};
use gradient_entity::project_cache::CacheSubscriptionMode;
use gradient_entity::task_trigger::TriggerType;
use gradient_entity::{
    build_attempt, build_job, cache_upstream, cluster_attempt, cluster_job, cluster_member,
    derivation, derivation_build, derivation_feature, derivation_metric, dispatched_job,
    dispatched_job_phase, eval_cache_store, eval_split, evaluation,
    evaluation_flake_input_override, evaluation_input_update, evaluation_metric, feature,
    task_action, task_flake_input_override, task_trigger,
};
use gradient_types::{ActionConfig, ActionType, PatchGeneratorKind, PrGranularity, VerifyGate};

use super::Shape;
use crate::{rows, test_date};

pub fn eval_job_key(evaluation: EvaluationId) -> String {
    format!("eval:{evaluation}")
}

impl Shape {
    pub fn evaluation_option(mut self, change: impl FnOnce(&mut evaluation::Model)) -> Self {
        let at = self.current_evaluation();
        change(&mut self.evaluations[at]);
        self
    }

    pub fn repository(mut self, url: &str) -> Self {
        let (at, task) = (self.current_evaluation(), self.current_task());
        self.evaluations[at].repository = url.into();
        self.tasks[task].repository = url.into();
        self
    }

    pub fn commit_hash(mut self, hash: &[u8]) -> Self {
        let commit = self.evaluations[self.current_evaluation()].commit;
        if let Some(row) = self.commits.iter_mut().find(|c| c.id == commit) {
            row.hash = hash.to_vec();
        }
        self
    }

    pub fn wildcard(self, wildcard: &str) -> Self {
        self.evaluation_option(|e| e.wildcard = wildcard.into())
    }

    pub fn branch(self, branch: &str) -> Self {
        self.evaluation_option(|e| e.branch = branch.into())
    }

    pub fn without_commit(self) -> Self {
        self.evaluation_option(|e| e.commit = CommitId::now_v7())
    }

    pub fn without_task(self) -> Self {
        self.evaluation_option(|e| e.task = None)
    }

    pub fn evaluation_kind(self, kind: EvaluationKind) -> Self {
        self.evaluation_option(|e| e.kind = kind)
    }

    pub fn waiting_reason(self, reason: serde_json::Value) -> Self {
        self.evaluation_option(|e| e.waiting_reason = Some(reason))
    }

    pub fn updated_at(self, at: NaiveDateTime) -> Self {
        self.evaluation_option(|e| e.updated_at = at)
    }

    pub fn last_evaluation(self, evaluation: EvaluationId) -> Self {
        self.task_option(|task| task.last_evaluation = Some(evaluation))
    }

    pub fn trigger(
        mut self,
        id: TaskTriggerId,
        kind: TriggerType,
        config: serde_json::Value,
        active: bool,
        last_fired_at: Option<NaiveDateTime>,
    ) -> Self {
        let task = self.tasks[self.current_task()].id;
        let is_default = !self.triggers.iter().any(|t| t.task == task);
        self.triggers.push(task_trigger::Model {
            id,
            task,
            trigger_type: kind,
            config,
            active,
            is_default,
            last_fired_at,
            created_at: test_date(),
            updated_at: test_date(),
        });
        self
    }

    pub fn flake_update_pull_request(mut self, inputs: &[&str]) -> Self {
        let task = self.tasks[self.current_task()].id;
        let created_by = self.owner();
        let config = ActionConfig::OpenPr {
            integration_id: IntegrationId::now_v7(),
            generator: PatchGeneratorKind::FlakeLock,
            granularity: PrGranularity::PerRun,
            verify_gate: VerifyGate::Build,
            branch_pattern: "gradient/flake-lock-update".into(),
            title_template: None,
            body_template: None,
            update_existing: true,
        };
        self.actions.push(task_action::Model {
            id: TaskActionId::now_v7(),
            task,
            name: "flake-update".into(),
            action_type: ActionType::OpenPr,
            config: serde_json::to_value(config).expect("an open_pr config"),
            events: serde_json::json!([]),
            active: true,
            last_fired_at: None,
            created_by,
            created_at: test_date(),
            updated_at: test_date(),
        });
        self.tracked_inputs
            .extend(inputs.iter().map(|input| task_flake_input_override::Model {
                id: FlakeInputOverrideId::now_v7(),
                task,
                input_name: (*input).into(),
                url: None,
                created_at: test_date(),
                updated_at: test_date(),
            }));
        self
    }

    pub fn input_override(mut self, input: &str, url: &str) -> Self {
        let evaluation = self.evaluations[self.current_evaluation()].id;
        self.input_overrides
            .push(evaluation_flake_input_override::Model {
                id: EvaluationFlakeInputOverrideId::now_v7(),
                evaluation,
                input_name: input.into(),
                url: Some(url.into()),
            });
        self
    }

    pub fn input_update(
        mut self,
        base_commit: &str,
        generator: &str,
        targets: &[&str],
        discover_only: bool,
    ) -> Self {
        let evaluation = self.evaluations[self.current_evaluation()].id;
        self.input_updates.push(evaluation_input_update::Model {
            id: EvaluationInputUpdateId::now_v7(),
            evaluation,
            base_commit: base_commit.into(),
            generator: generator.into(),
            target_inputs: serde_json::json!(targets),
            discover_only,
            created_at: test_date(),
            updated_at: test_date(),
            ..Default::default()
        });
        self
    }

    pub fn worker_registration(mut self, worker_id: &str) -> Self {
        let owner = self.owner();
        let project = self.projects[self.current_task()].id;
        self.registrations
            .push(gradient_entity::worker_registration::Model {
                peer_id: project,
                worker_id: worker_id.into(),
                token_hash: format!("token-{worker_id}"),
                created_by: Some(owner),
                created_at: test_date(),
                ..rows::worker_registration(true, true)
            });
        self
    }

    pub fn evaluation_metric(mut self, change: impl FnOnce(&mut evaluation_metric::Model)) -> Self {
        let evaluation = self.evaluations[self.current_evaluation()].id;
        let mut metric = evaluation_metric::Model {
            id: EvaluationMetricId::now_v7(),
            evaluation,
            worker_id: "worker".into(),
            created_at: test_date(),
            ..Default::default()
        };
        change(&mut metric);
        self.evaluation_metrics.push(metric);
        self
    }

    pub fn derivation_option(mut self, change: impl FnOnce(&mut derivation::Model)) -> Self {
        let at = self.current_derivation();
        change(&mut self.derivations[at]);
        self
    }

    pub fn shared_build_option(
        mut self,
        change: impl FnOnce(&mut derivation_build::Model),
    ) -> Self {
        let at = self.current_derivation();
        change(&mut self.shared_builds[at]);
        self
    }

    pub fn job_option(mut self, change: impl FnOnce(&mut build_job::Model)) -> Self {
        let (evaluation, derivation) = (
            self.evaluations[self.current_evaluation()].id,
            self.derivations[self.current_derivation()].id,
        );
        let job = self
            .jobs
            .iter_mut()
            .find(|j| j.evaluation == evaluation && j.derivation == derivation)
            .unwrap_or_else(|| panic!("declare the build job of {derivation} first"));
        change(job);
        self
    }

    pub fn system(self, architecture: &str) -> Self {
        self.derivation_option(|d| d.architecture = architecture.into())
    }

    pub fn fixed_output(self) -> Self {
        self.derivation_option(|d| d.is_fixed_output = true)
    }

    pub fn pname(self, pname: &str) -> Self {
        self.derivation_option(|d| d.pname = Some(pname.into()))
    }

    pub fn output_option(
        mut self,
        hash: &str,
        change: impl FnOnce(&mut gradient_entity::derivation_output::Model),
    ) -> Self {
        let derivation = self.derivations[self.current_derivation()].id;
        let at = self
            .outputs
            .iter()
            .position(|o| o.derivation == derivation && o.hash.as_deref() == Some(hash))
            .unwrap_or_else(|| panic!("declare output {hash} first"));
        change(&mut self.outputs[at]);
        self
    }

    pub fn feature(mut self, name: &str) -> Self {
        let derivation = self.derivations[self.current_derivation()].id;
        let feature = match self.features.iter().find(|f| f.name == name) {
            Some(declared) => declared.id,
            None => {
                let id = FeatureId::now_v7();
                self.features.push(feature::Model {
                    id,
                    name: name.into(),
                    kind: Default::default(),
                });
                id
            }
        };
        self.derivation_features.push(derivation_feature::Model {
            id: DerivationFeatureId::now_v7(),
            derivation,
            feature,
        });
        self
    }

    pub fn derivation_metric(mut self, change: impl FnOnce(&mut derivation_metric::Model)) -> Self {
        let declared = &self.derivations[self.current_derivation()];
        let mut metric = derivation_metric::Model {
            id: DerivationMetricId::now_v7(),
            derivation: declared.id,
            pname: Some(
                declared
                    .pname
                    .clone()
                    .unwrap_or_else(|| declared.name.clone()),
            ),
            architecture: declared.architecture.clone(),
            worker_id: "worker".into(),
            created_at: test_date(),
            ..Default::default()
        };
        change(&mut metric);
        self.derivation_metrics.push(metric);
        self
    }

    pub fn upstream(
        mut self,
        id: CacheUpstreamId,
        url: &str,
        public_key: &str,
        kind: CacheUpstreamKind,
        mode: CacheSubscriptionMode,
        active: bool,
    ) -> Self {
        let cache = self.caches[self.cache.expect("name a cache first")].id;
        self.upstreams.push(cache_upstream::Model {
            id,
            cache,
            display_name: format!("upstream-{}", id.into_inner().simple()),
            mode,
            kind,
            url: Some(url.into()),
            public_key: Some(public_key.into()),
            active,
            ..Default::default()
        });
        self
    }

    pub fn eval_cache(mut self, fingerprint: &str) -> Self {
        let cache = self.caches[self.cache.expect("name a cache first")].id;
        self.eval_caches.push(eval_cache_store::Model {
            id: EvalCacheStoreId::now_v7(),
            cache,
            fingerprint: fingerprint.into(),
            storage_path: format!("eval-cache-segments/{cache}/{fingerprint}"),
            size_bytes: 1,
            created_at: test_date(),
            updated_at: test_date(),
            purged_at: None,
        });
        self
    }

    pub fn eval_assignment(mut self) -> Self {
        let (evaluation, task) = (self.current_evaluation(), self.current_task());
        let row = eval_assignment(
            self.evaluations[evaluation].id,
            self.projects[task].id,
            self.tasks[task].id,
            test_date(),
        );
        self.assignments.push(row);
        self.assignment = Some(self.assignments.len() - 1);
        self
    }

    pub fn assignment_option(mut self, change: impl FnOnce(&mut dispatched_job::Model)) -> Self {
        let at = self.assignment.expect("declare an assignment first");
        change(&mut self.assignments[at]);
        self
    }

    pub fn by_worker(self, worker: &str) -> Self {
        self.assignment_option(|a| a.worker_id = worker.into())
    }

    pub fn dispatched_at(self, at: NaiveDateTime) -> Self {
        self.assignment_option(|a| {
            a.queued_at = at;
            a.dispatched_at = at;
            a.created_at = at;
        })
    }

    pub fn ready_at(self, at: NaiveDateTime) -> Self {
        self.assignment_option(|a| a.ready_at = Some(at))
    }

    pub fn closed(self, outcome: DispatchedJobOutcome, at: NaiveDateTime) -> Self {
        self.assignment_option(|a| {
            a.finished_at = Some(at);
            a.outcome = Some(outcome);
        })
    }

    pub fn claim_window(
        self,
        missing_count: i32,
        missing_nar_size: i64,
        dependency_count: i32,
    ) -> Self {
        self.assignment_option(|a| {
            a.missing_count = Some(missing_count);
            a.missing_nar_size = Some(missing_nar_size);
            a.dependency_count = Some(dependency_count);
        })
    }

    pub fn worker_elapsed_ms(self, elapsed_ms: i64) -> Self {
        self.assignment_option(|a| a.worker_elapsed_ms = Some(elapsed_ms))
    }

    pub fn phase(mut self, phase: i16, start_ms: i64, end_ms: i64) -> Self {
        let dispatched_job =
            self.assignments[self.assignment.expect("declare an assignment first")].id;
        let seq = self
            .phases
            .iter()
            .filter(|p| p.dispatched_job == dispatched_job)
            .count();
        self.phases.push(dispatched_job_phase::Model {
            id: DispatchedJobPhaseId::now_v7(),
            dispatched_job,
            seq: i32::try_from(seq).unwrap_or(i32::MAX),
            phase,
            start_ms,
            end_ms,
            created_at: test_date(),
            ..Default::default()
        });
        self
    }

    pub fn cluster(
        mut self,
        id: ClusterJobId,
        same_zone: bool,
        retry_budget: i32,
        status: ClusterJobStatus,
        attempts: i32,
        created_at: NaiveDateTime,
    ) -> Self {
        self.clusters.push(cluster_job::Model {
            id,
            status,
            same_zone,
            attempts,
            retry_budget,
            created_at,
            updated_at: created_at,
            dissolve_after: None,
        });
        self
    }

    pub fn cluster_option(
        mut self,
        id: ClusterJobId,
        change: impl FnOnce(&mut cluster_job::Model),
    ) -> Self {
        let at = self
            .clusters
            .iter()
            .position(|c| c.id == id)
            .unwrap_or_else(|| panic!("declare cluster {id} first"));
        change(&mut self.clusters[at]);
        self
    }

    pub fn evaluation_member_of(
        mut self,
        cluster: ClusterJobId,
        role: &str,
        primary: bool,
        pin: Option<&str>,
    ) -> Self {
        let evaluation = self.evaluations[self.current_evaluation()].id;
        self.members.push(member(cluster, role, primary, pin));
        if let Some(added) = self.members.last_mut() {
            added.evaluation = Some(evaluation);
        }
        self
    }

    pub fn split_member_of(mut self, cluster: ClusterJobId, role: &str, eval_index: i16) -> Self {
        let evaluation = self.evaluations[self.current_evaluation()].id;
        self.members.push(cluster_member::Model {
            evaluation: Some(evaluation),
            eval_index: Some(eval_index),
            ..member(cluster, role, eval_index == 0, None)
        });
        self
    }

    pub fn eval_split_of(mut self, cluster: ClusterJobId) -> Self {
        let evaluation = self.evaluations[self.current_evaluation()].id;
        self.eval_splits.push(eval_split::Model {
            evaluation,
            cluster_job: cluster,
            ..Default::default()
        });
        self
    }

    pub fn build_member_of(
        mut self,
        cluster: ClusterJobId,
        role: &str,
        primary: bool,
        pin: Option<&str>,
    ) -> Self {
        let shared_build = self.shared_builds[self.current_derivation()].id;
        self.members.push(member(cluster, role, primary, pin));
        if let Some(added) = self.members.last_mut() {
            added.derivation_build = Some(shared_build);
        }
        self
    }

    pub fn cluster_attempt(
        mut self,
        cluster: ClusterJobId,
        id: ClusterAttemptId,
        started_at: Option<NaiveDateTime>,
        finished_at: Option<NaiveDateTime>,
        outcome: Option<ClusterAttemptOutcome>,
    ) -> Self {
        self.cluster_attempts.push(cluster_attempt::Model {
            id,
            cluster_job: cluster,
            created_at: started_at.unwrap_or_else(test_date),
            started_at,
            finished_at,
            outcome,
        });
        self
    }

    pub(super) fn open_member_rows(&mut self) {
        let open: Vec<(ClusterAttemptId, ClusterJobId, NaiveDateTime)> = self
            .cluster_attempts
            .iter()
            .filter(|a| a.finished_at.is_none())
            .map(|a| (a.id, a.cluster_job, a.created_at))
            .collect();
        for (attempt, cluster, at) in open {
            let rows: Vec<dispatched_job::Model> = self
                .members
                .iter()
                .filter(|m| m.cluster_job == cluster)
                .enumerate()
                .filter_map(|(index, m)| self.member_row(m, index, at))
                .map(|row| dispatched_job::Model {
                    cluster_attempt: Some(attempt),
                    ..row
                })
                .collect();
            for row in &rows {
                self.running_attempt_of(row, at);
            }
            self.assignments.extend(rows);
        }
    }

    fn running_attempt_of(&mut self, row: &dispatched_job::Model, at: NaiveDateTime) {
        let Some(build) = self
            .shared_builds
            .iter()
            .find(|b| Some(super::build_job_key(b.id)) == row.job_id)
        else {
            return;
        };
        let (shared_build, derivation) = (build.id, build.derivation);
        let build_job = self
            .jobs
            .iter()
            .find(|j| j.evaluation == row.evaluation_id && j.derivation == derivation)
            .map(|j| j.id);
        self.attempts.push(build_attempt::Model {
            id: BuildAttemptId::now_v7(),
            build_job,
            derivation_build: shared_build,
            dispatched_job: Some(row.id),
            outcome: AttemptOutcome::Running,
            build_context: serde_json::json!({}),
            created_at: at,
            ..Default::default()
        });
    }

    fn member_row(
        &self,
        member: &cluster_member::Model,
        index: usize,
        at: NaiveDateTime,
    ) -> Option<dispatched_job::Model> {
        let worker = format!("member-{index}");
        if let Some(evaluation) = member.evaluation {
            let (project, task) = self.project_and_task_of(evaluation)?;
            return Some(dispatched_job::Model {
                worker_id: worker,
                ..eval_assignment(evaluation, project, task, at)
            });
        }
        let shared_build = member.derivation_build?;
        let evaluation = self
            .jobs
            .iter()
            .find(|j| j.derivation_build == shared_build)?
            .evaluation;
        let (project, task) = self.project_and_task_of(evaluation)?;
        Some(dispatched_job::Model {
            worker_id: worker,
            ..super::build_assignment(evaluation, project, task, shared_build, at)
        })
    }

    fn project_and_task_of(&self, evaluation: EvaluationId) -> Option<(ProjectId, TaskId)> {
        let at = self.evaluations.iter().position(|e| e.id == evaluation)?;
        let task = &self.tasks[self.task_of[at]];
        Some((task.project, task.id))
    }
}

fn member(
    cluster: ClusterJobId,
    role: &str,
    primary: bool,
    pin: Option<&str>,
) -> cluster_member::Model {
    cluster_member::Model {
        id: ClusterMemberId::now_v7(),
        cluster_job: cluster,
        evaluation: None,
        derivation_build: None,
        role: role.into(),
        primary,
        pin: pin.map(str::to_owned),
        ..Default::default()
    }
}

pub fn eval_assignment(
    evaluation: EvaluationId,
    project: ProjectId,
    task: TaskId,
    at: NaiveDateTime,
) -> dispatched_job::Model {
    dispatched_job::Model {
        id: DispatchedJobId::now_v7(),
        kind: DispatchedJobKind::Eval,
        job_id: Some(eval_job_key(evaluation)),
        ..super::build_assignment(evaluation, project, task, Default::default(), at)
    }
}
