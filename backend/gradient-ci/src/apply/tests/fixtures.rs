/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::super::ApplyInput;
use gradient_entity::evaluation::{EvaluationStatus, WalkMode};
use gradient_fixtures::rows::worker_registration;
use gradient_types::ids::{CacheId, ProjectCacheId, UserId};
use gradient_types::triggers::TriggerType;
use gradient_types::*;
use sea_orm::{DbErr, MockDatabase, RuntimeErr, sqlx};
use std::sync::Arc;
use uuid::Uuid;

pub fn make_task_with_last_eval(last: Option<EvaluationId>) -> MTask {
    make_task_with_concurrency(last, ConcurrencyPolicy::Skip)
}

pub fn make_task_with_concurrency(
    last: Option<EvaluationId>,
    concurrency: ConcurrencyPolicy,
) -> MTask {
    MTask {
        id: TaskId::new(Uuid::parse_str("00000000-0000-0000-0000-000000000003").unwrap()),
        project: ProjectId::nil(),
        name: "test-task".into(),
        active: true,
        display_name: "Test".into(),
        repository: "https://example/r".into(),
        wildcard: "*".into(),
        last_evaluation: last,
        created_by: UserId::nil(),
        keep_evaluations: 10,
        concurrency,
        sign_cache: true,
        ..Default::default()
    }
}

pub fn make_eval(
    id: EvaluationId,
    task: TaskId,
    commit: CommitId,
    status: EvaluationStatus,
) -> gradient_entity::evaluation::Model {
    gradient_entity::evaluation::Model {
        id,
        task: Some(task),
        commit,
        wildcard: "*".into(),
        status,
        ..Default::default()
    }
}

pub fn make_commit(id: CommitId, hash: Vec<u8>) -> gradient_entity::commit::Model {
    gradient_entity::commit::Model {
        id,
        hash,
        ..Default::default()
    }
}

pub fn input(trig: TaskTriggerId, ttype: TriggerType, hash: Vec<u8>, manual: bool) -> ApplyInput {
    ApplyInput {
        trigger_id: trig,
        trigger_type: ttype,
        branch: String::new(),
        commit_hash: hash,
        commit_message: None,
        author_name: None,
        manual,
        honors_skip_marker: false,
        gate_approval: None,
        repository_override: None,
        wildcard_override: None,
        source_comment: None,
        started_by: None,
        walk_mode: WalkMode::Pruned,
        instance_max_storage_gb: 0,
    }
}

fn cache_row(active: bool) -> gradient_entity::cache::Model {
    gradient_entity::cache::Model {
        id: CacheId::now_v7(),
        name: "cache".into(),
        display_name: "Cache".into(),
        active,
        priority: 10,
        created_by: UserId::nil(),
        ..Default::default()
    }
}

fn project_cache_row(cache: CacheId) -> gradient_entity::project_cache::Model {
    gradient_entity::project_cache::Model {
        id: ProjectCacheId::now_v7(),
        project: ProjectId::nil(),
        cache,
        mode: gradient_entity::project_cache::CacheSubscriptionMode::ReadWrite,
    }
}

pub fn with_writable_cache(db: MockDatabase) -> MockDatabase {
    let cache = cache_row(true);
    db.append_query_results([vec![project_cache_row(cache.id)]])
        .append_query_results([vec![cache]])
}

pub fn with_storage_not_full(db: MockDatabase) -> MockDatabase {
    db.append_query_results([Vec::<gradient_entity::project_cache::Model>::new()])
}

pub fn with_eval_worker(db: MockDatabase) -> MockDatabase {
    db.append_query_results([vec![worker_registration(true, true)]])
}

#[derive(Debug)]
struct ActiveEvaluationExists;

impl std::fmt::Display for ActiveEvaluationExists {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("duplicate key value violates unique constraint")
    }
}

impl std::error::Error for ActiveEvaluationExists {}

impl sqlx::error::DatabaseError for ActiveEvaluationExists {
    fn message(&self) -> &str {
        "duplicate key value violates unique constraint"
    }

    fn as_error(&self) -> &(dyn std::error::Error + Send + Sync + 'static) {
        self
    }

    fn as_error_mut(&mut self) -> &mut (dyn std::error::Error + Send + Sync + 'static) {
        self
    }

    fn into_error(self: Box<Self>) -> Box<dyn std::error::Error + Send + Sync + 'static> {
        self
    }

    fn kind(&self) -> sqlx::error::ErrorKind {
        sqlx::error::ErrorKind::UniqueViolation
    }

    fn constraint(&self) -> Option<&str> {
        Some("uq_evaluation_one_active_per_branch")
    }
}

pub fn one_active_per_branch_violation() -> DbErr {
    DbErr::Query(RuntimeErr::SqlxError(Arc::new(sqlx::Error::Database(
        Box::new(ActiveEvaluationExists),
    ))))
}
