/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::{self, EvaluationStatus};
use gradient_types::*;
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
        concurrency: ConcurrencyPolicy::Skip,
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

pub fn make_entry_point(eval_id: EvaluationId, derivation: DerivationId) -> MEntryPoint {
    MEntryPoint {
        id: EntryPointId::now_v7(),
        project: ProjectId::new(Uuid::parse_str("00000000-0000-0000-0000-000000000003").unwrap()),
        evaluation: eval_id,
        derivation,
        eval: "default".into(),
        ..Default::default()
    }
}

pub fn make_anchor(derivation: DerivationId, status: BuildStatus) -> MDerivationBuild {
    MDerivationBuild {
        id: DerivationBuildId::now_v7(),
        derivation,
        status,
        ..Default::default()
    }
}
