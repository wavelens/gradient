/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::ids::{CommitId, DerivationId, DerivationOutputId, EvaluationId, TaskId};
use gradient_entity::{commit, derivation, derivation_dependency, derivation_output, evaluation};

use crate::ids::user_id;

pub fn derivation_dependency(
    derivation: DerivationId,
    dependency: DerivationId,
) -> derivation_dependency::Model {
    derivation_dependency::Model {
        derivation,
        dependency,
        ..Default::default()
    }
}

pub fn derivation_output(
    derivation: DerivationId,
    hash: &str,
    nar_size: Option<i64>,
) -> derivation_output::Model {
    derivation_output::Model {
        id: DerivationOutputId::now_v7(),
        derivation,
        name: "out".into(),
        hash: hash.into(),
        package: "foo".into(),
        nar_size,
        created_at: chrono::Utc::now().naive_utc(),
        ..Default::default()
    }
}

pub fn build_request_commit() -> commit::Model {
    commit::Model {
        id: CommitId::now_v7(),
        message: "Build request".into(),
        hash: vec![0; 20],
        author: Some(user_id()),
        author_name: "Test User".into(),
    }
}

pub fn queued_evaluation(task: TaskId, commit: CommitId) -> evaluation::Model {
    let now = chrono::Utc::now().naive_utc();
    evaluation::Model {
        id: EvaluationId::now_v7(),
        task: Some(task),
        repository: "/nix/store/abc-source".into(),
        commit,
        wildcard: "*".into(),
        status: evaluation::EvaluationStatus::Queued,
        created_at: now,
        updated_at: now,
        ..Default::default()
    }
}

pub fn derivation_named(id: DerivationId, name: &str) -> derivation::Model {
    derivation::Model {
        id,
        hash: format!("hash{name}"),
        name: name.into(),
        architecture: "x86_64-linux".into(),
        created_at: chrono::Utc::now().naive_utc(),
        ..Default::default()
    }
}
