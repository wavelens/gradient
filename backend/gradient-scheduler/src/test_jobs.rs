/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;

use gradient_pool::session_port::{SessionPort, SessionSignal};
use gradient_types::ids::*;
use gradient_wire::types::{
    BuildJob, BuildSpec, BuildSpecKind, FlakeJob, FlakeStep, GradientCapabilities,
};
use tokio::sync::mpsc;

use crate::jobs::{PendingBuildJob, PendingEvalJob, PendingRunnerJob};
use gradient_pool::WorkerCaps;
use gradient_types::runner::{RunnerAllow, RunnerDefaults, RunnerInventory, RunnerSpec};

pub(crate) fn port() -> (Arc<dyn SessionPort>, mpsc::UnboundedReceiver<SessionSignal>) {
    let (tx, rx) = mpsc::unbounded_channel();
    (Arc::new(tx), rx)
}

pub(crate) fn eval_job(peer: ProjectId) -> PendingEvalJob {
    PendingEvalJob {
        evaluation_id: EvaluationId::now_v7(),
        task_id: None,
        project_id: peer,
        commit_id: CommitId::now_v7(),
        repository: "https://example.com/repo".into(),
        job: FlakeJob {
            steps: vec![FlakeStep::EvaluateDerivations],
            source: gradient_wire::types::FlakeSource::Repository {
                url: "https://example.com/repo".into(),
                commit: "abc123".into(),
            },
            wildcards: vec!["*".into()],
            timeout_secs: None,
            input_overrides: vec![],
            input_update: None,
        },
        required_paths: vec![],
        queued_at: gradient_types::now(),
        ready_at: gradient_types::now(),
        rescore_count: 0,
        prioritized: false,
        build_request: false,
        estimate: Default::default(),
        walk_mode: Default::default(),
        eval_index: 0,
    }
}

pub(crate) fn build_job(
    evaluation_id: EvaluationId,
    peer: ProjectId,
    derivation_build: DerivationBuildId,
) -> PendingBuildJob {
    PendingBuildJob {
        derivation_build,
        derivation: DerivationId::now_v7(),
        evaluation_id,
        project_id: peer,
        job: BuildJob {
            build: BuildSpec {
                build_id: derivation_build.to_string(),
                drv_path: "aaaa-hello.drv".into(),
                kind: BuildSpecKind::Build,
                is_fixed_output: false,
                outputs: vec![],
                timeout_secs: None,
                max_silent_secs: None,
                build_cores: None,
                input_realisations: Vec::new(),
            },
            requirement: gradient_wire::types::BuildRequirement {
                architecture: "x86_64-linux".into(),
                required_features: vec![],
            },
        },
        required_paths: vec![],
        dependency_count: 0,
        closure_size: None,
        prefer_local_build: false,
        is_fixed_output: false,
        estimate: Default::default(),
        history_key: None,
        queued_at: gradient_types::now(),
        ready_at: gradient_types::now(),
        rescore_count: 0,
        prioritized: false,
        build_request: false,
        ifd: false,
        pname: None,
        substitute: false,
    }
}

pub(crate) fn eval_worker_caps() -> GradientCapabilities {
    GradientCapabilities {
        eval: true,
        ..GradientCapabilities::default()
    }
}

pub(crate) fn runner_job(spec: RunnerSpec) -> PendingRunnerJob {
    PendingRunnerJob {
        runner_job: RunnerJobId::now_v7(),
        evaluation_id: EvaluationId::now_v7(),
        project_id: ProjectId::now_v7(),
        task_id: TaskId::now_v7(),
        entry_point: "apps.x86_64-linux.train".into(),
        repository: "git+ssh://git@example.com/repo".into(),
        commit: "abc123".into(),
        architecture: "x86_64-linux".into(),
        retries_left: spec.retries,
        spec,
        image_path: None,
        attempts_done: 0,
        last_phase: None,
        required_paths: vec![],
        queued_at: gradient_types::now(),
        ready_at: gradient_types::now(),
        rescore_count: 0,
        api_url: "http://127.0.0.1:3000".into(),
        assignment: None,
    }
}

pub(crate) fn allow_all() -> RunnerAllow {
    RunnerAllow {
        network: true,
        ssh: true,
        ports: (1, u16::MAX),
        nodes: 1,
        custom_images: true,
        ..RunnerAllow::default()
    }
}

pub(crate) fn runner_caps(allow: RunnerAllow, defaults: RunnerDefaults) -> WorkerCaps {
    WorkerCaps {
        architectures: vec!["x86_64-linux".into()],
        capabilities: GradientCapabilities {
            exec: true,
            ..GradientCapabilities::default()
        },
        runner: Some(RunnerInventory {
            labels: vec!["w1".into()],
            allow,
            defaults,
            ..RunnerInventory::default()
        }),
        ..WorkerCaps::default()
    }
}
