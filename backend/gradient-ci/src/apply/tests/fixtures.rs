/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::super::ApplyInput;
use gradient_types::ids::{CacheId, OrganizationCacheId, UserId, WorkerRegistrationId};
use gradient_types::triggers::TriggerType;
use gradient_types::*;
use gradient_entity::evaluation::EvaluationStatus;
use sea_orm::MockDatabase;
use uuid::Uuid;

pub fn make_project_with_last_eval(last: Option<EvaluationId>) -> MProject {
    make_project_with_concurrency(last, ConcurrencyPolicy::Skip)
}

pub fn make_project_with_concurrency(
    last: Option<EvaluationId>,
    concurrency: ConcurrencyPolicy,
) -> MProject {
    MProject {
        id: ProjectId::new(Uuid::parse_str("00000000-0000-0000-0000-000000000003").unwrap()),
        organization: OrganizationId::nil(),
        name: "test-project".into(),
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
    project: ProjectId,
    commit: CommitId,
    status: EvaluationStatus,
) -> gradient_entity::evaluation::Model {
    gradient_entity::evaluation::Model {
        id,
        project: Some(project),
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

pub fn input(
    trig: ProjectTriggerId,
    ttype: TriggerType,
    hash: Vec<u8>,
    manual: bool,
) -> ApplyInput {
    ApplyInput {
        trigger_id: trig,
        trigger_type: ttype,
        commit_hash: hash,
        commit_message: None,
        author_name: None,
        manual,
        gate_approval: None,
        repository_override: None,
        wildcard_override: None,
        source_comment: None,
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

fn org_cache_row(cache: CacheId) -> gradient_entity::organization_cache::Model {
    gradient_entity::organization_cache::Model {
        id: OrganizationCacheId::now_v7(),
        organization: OrganizationId::nil(),
        cache,
        mode: gradient_entity::organization_cache::CacheSubscriptionMode::ReadWrite,
    }
}

/// Append the two queries `org_has_writable_cache` issues for the
/// "writable cache exists" path.
pub fn with_writable_cache(db: MockDatabase) -> MockDatabase {
    let cache = cache_row(true);
    db.append_query_results([vec![org_cache_row(cache.id)]])
        .append_query_results([vec![cache]])
}

/// Append the single query `park_if_storage_full` issues for the "not
/// full" path: `org_writable_caches` finds no org_cache rows, so
/// `org_caches_all_full` short-circuits to `false`.
pub fn with_storage_not_full(db: MockDatabase) -> MockDatabase {
    db.append_query_results([Vec::<gradient_entity::organization_cache::Model>::new()])
}

fn worker_registration_row(
    active: bool,
    enable_eval: bool,
) -> gradient_entity::worker_registration::Model {
    gradient_entity::worker_registration::Model {
        id: WorkerRegistrationId::now_v7(),
        peer_id: OrganizationId::nil(),
        worker_id: "00000000-0000-4000-8000-000000000001".into(),
        active,
        enable_fetch: true,
        enable_eval,
        enable_build: true,
        created_by: Some(UserId::nil()),
        ..Default::default()
    }
}

/// Append the single query `org_has_eval_capable_worker_registration`
/// issues for the "eval-capable worker exists" path.
pub fn with_eval_worker(db: MockDatabase) -> MockDatabase {
    db.append_query_results([vec![worker_registration_row(true, true)]])
}
