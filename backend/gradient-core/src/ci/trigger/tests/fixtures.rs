/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::types::*;
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::{self, EvaluationStatus};
use uuid::Uuid;

pub fn make_project() -> MProject {
    MProject {
        id: ProjectId::new(Uuid::parse_str("00000000-0000-0000-0000-000000000003").unwrap()),
        organization: OrganizationId::nil(),
        name: "test-project".into(),
        active: true,
        display_name: "Test Project".into(),
        repository: "https://github.com/test/repo".into(),
        wildcard: "*".into(),
        created_by: UserId::nil(),
        keep_evaluations: 10,
        concurrency: 3,
        sign_cache: true,
        ..Default::default()
    }
}

pub fn make_eval(id: EvaluationId, status: EvaluationStatus) -> evaluation::Model {
    evaluation::Model {
        id,
        project: Some(ProjectId::new(
            Uuid::parse_str("00000000-0000-0000-0000-000000000003").unwrap(),
        )),
        repository: "https://github.com/test/repo".into(),
        commit: CommitId::nil(),
        wildcard: "*".into(),
        status,
        ..Default::default()
    }
}

pub fn make_build(
    id: BuildId,
    eval_id: EvaluationId,
    status: BuildStatus,
) -> gradient_entity::build::Model {
    make_build_drv(id, eval_id, DerivationId::now_v7(), status)
}

pub fn make_build_drv(
    id: BuildId,
    eval_id: EvaluationId,
    derivation: DerivationId,
    status: BuildStatus,
) -> gradient_entity::build::Model {
    gradient_entity::build::Model {
        id,
        evaluation: eval_id,
        derivation,
        status,
        ..Default::default()
    }
}
