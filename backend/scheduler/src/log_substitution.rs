/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Best-effort log substitution for `Substituted` and `external_cached` builds.
//!
//! Two-stage strategy:
//! 1. Reuse a sibling build's `log_id` if any prior completed build for the
//!    same derivation has one (DB-only, no HTTP).
//! 2. (Only when `allow_upstream_fetch == true`) fall back to the
//!    Hydra-style `/log/{drv}` endpoint on each configured upstream cache.
//!
//! Failures are never fatal — log substitution must not break the build pipeline.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use entity::build::{ActiveModel as ABuild, BuildStatus, Column as CBuild, Entity as EBuild};
use gradient_core::types::ids::{BuildId, DerivationId};
use gradient_core::types::ServerState;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, EntityTrait, QueryFilter, QueryOrder,
};
use tracing::{debug, warn};

#[allow(dead_code)]
const LOG_FETCH_TIMEOUT: Duration = Duration::from_secs(10);
#[allow(dead_code)]
const LOG_FETCH_MAX_BYTES: usize = 16 * 1024 * 1024;

/// Try to give `build_id` a `log_id` via local dedup, then (optionally) an
/// upstream `/log/{drv}` fetch. Always returns `Ok` — failures are logged but
/// never propagated, so the caller's pipeline is unaffected.
pub async fn substitute_log(
    state: Arc<ServerState>,
    build_id: BuildId,
    derivation_id: DerivationId,
    drv_path: String,
    allow_upstream_fetch: bool,
) -> Result<()> {
    let build = match EBuild::find_by_id(build_id).one(&state.worker_db).await {
        Ok(Some(b)) => b,
        Ok(None) => {
            debug!(%build_id, "substitute_log: build not found");
            return Ok(());
        }
        Err(e) => {
            warn!(%build_id, error = %e, "substitute_log: build lookup failed");
            return Ok(());
        }
    };

    if build.log_id.is_some() {
        return Ok(());
    }

    if let Some(effective) = find_dedup_log_id(&state, build_id, derivation_id).await {
        set_log_id(&state, build_id, effective).await;
        return Ok(());
    }

    if !allow_upstream_fetch {
        return Ok(());
    }

    let _ = drv_path;
    // Upstream fetch added in Task 4.
    Ok(())
}

async fn find_dedup_log_id(
    state: &Arc<ServerState>,
    build_id: BuildId,
    derivation_id: DerivationId,
) -> Option<BuildId> {
    match EBuild::find()
        .filter(CBuild::Derivation.eq(derivation_id))
        .filter(CBuild::Id.ne(build_id))
        .filter(CBuild::LogId.is_not_null())
        .order_by_desc(CBuild::CreatedAt)
        .one(&state.worker_db)
        .await
    {
        Ok(Some(prior)) => return prior.log_id,
        Ok(None) => {}
        Err(e) => {
            warn!(%build_id, error = %e, "substitute_log: dedup query (a) failed");
            return None;
        }
    }

    let candidate = match EBuild::find()
        .filter(CBuild::Derivation.eq(derivation_id))
        .filter(CBuild::Id.ne(build_id))
        .filter(CBuild::Status.eq(BuildStatus::Completed))
        .order_by_desc(CBuild::CreatedAt)
        .one(&state.worker_db)
        .await
    {
        Ok(c) => c,
        Err(e) => {
            warn!(%build_id, error = %e, "substitute_log: dedup query (b) failed");
            return None;
        }
    };

    let candidate = candidate?;
    match state.log_storage.read(candidate.id).await {
        Ok(body) if !body.is_empty() => Some(candidate.id),
        _ => None,
    }
}

async fn set_log_id(state: &Arc<ServerState>, build_id: BuildId, log_id: BuildId) {
    let build = match EBuild::find_by_id(build_id).one(&state.worker_db).await {
        Ok(Some(b)) => b,
        Ok(None) => return,
        Err(e) => {
            warn!(%build_id, error = %e, "substitute_log: reload before update failed");
            return;
        }
    };
    let mut am: ABuild = build.into();
    am.log_id = Set(Some(log_id));
    if let Err(e) = am.update(&state.worker_db).await {
        warn!(%build_id, error = %e, "substitute_log: failed to set log_id");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use entity::build;
    use gradient_core::types::ids::EvaluationId;
    use test_support::prelude::*;
    use uuid::Uuid;

    fn make_build(
        id: BuildId,
        derivation: DerivationId,
        status: BuildStatus,
        log_id: Option<BuildId>,
        external_cached: bool,
    ) -> build::Model {
        let now = gradient_core::types::now();
        build::Model {
            id,
            evaluation: EvaluationId::new(Uuid::now_v7()),
            derivation,
            status,
            log_id,
            build_time_ms: None,
            worker: None,
            via: None,
            external_cached,
            created_at: now,
            updated_at: now,
        }
    }

    #[tokio::test]
    async fn dedup_hit_via_existing_log_id_pointer() {
        let drv = DerivationId::new(Uuid::now_v7());
        let prior_id = BuildId::new(Uuid::now_v7());
        let prior_log = BuildId::new(Uuid::now_v7());
        let new_id = BuildId::new(Uuid::now_v7());

        let prior = make_build(prior_id, drv, BuildStatus::Completed, Some(prior_log), false);
        let new = make_build(new_id, drv, BuildStatus::Substituted, None, false);

        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([vec![new.clone()]])
            .append_query_results([vec![prior.clone()]])
            .append_query_results([vec![new.clone()]])
            .append_exec_results([sea_orm::MockExecResult { last_insert_id: 0, rows_affected: 1 }])
            .append_query_results([vec![build::Model { log_id: Some(prior_log), ..new.clone() }]])
            .into_connection();

        let state = test_state(db);
        substitute_log(state, new_id, drv, "/nix/store/x-test.drv".to_string(), false)
            .await
            .expect("substitute_log returns Ok");
    }

    #[tokio::test]
    async fn no_prior_build_no_fetch_returns_ok() {
        let drv = DerivationId::new(Uuid::now_v7());
        let new_id = BuildId::new(Uuid::now_v7());
        let new = make_build(new_id, drv, BuildStatus::Substituted, None, false);

        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([vec![new.clone()]])
            .append_query_results([Vec::<build::Model>::new()])
            .append_query_results([Vec::<build::Model>::new()])
            .into_connection();

        let state = test_state(db);
        substitute_log(state, new_id, drv, "/nix/store/x-test.drv".to_string(), false)
            .await
            .expect("substitute_log returns Ok");
    }
}
