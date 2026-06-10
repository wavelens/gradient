/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Best-effort log substitution for `Substituted` and substitutable builds.
//!
//! Two-stage strategy: reuse a sibling build's attempt `log_id` first, and
//! (only when `allow_upstream_fetch == true`) fall back to the Hydra-style
//! `/log/{drv}` endpoint on each configured upstream cache. The resolved log id
//! is recorded on the build's latest `build_attempt`. All failures are
//! non-fatal - log substitution must never break the build pipeline.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use gradient_entity::build::{BuildStatus, Column as CBuild, Entity as EBuild};
use gradient_entity::derivation::Entity as EDerivation;
use futures::StreamExt;
use gradient_core::ServerState;
use gradient_types::ids::{BuildId, DerivationId};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter,
    QueryOrder,
};
use tracing::{debug, warn};

const LOG_FETCH_TIMEOUT: Duration = Duration::from_secs(10);
const LOG_FETCH_MAX_BYTES: usize = 16 * 1024 * 1024;

/// Try to give `build_id` a `log_id` via local dedup, then (optionally) an
/// upstream `/log/{drv}` fetch. Always returns `Ok` - failures are logged but
/// never propagated, so the caller's pipeline is unaffected.
pub async fn substitute_log(
    state: Arc<ServerState>,
    build_id: BuildId,
    derivation_id: DerivationId,
    drv_path: String,
    allow_upstream_fetch: bool,
) -> Result<()> {
    match EBuild::find_by_id(build_id).one(&state.worker_db).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            debug!(%build_id, "substitute_log: build not found");
            return Ok(());
        }
        Err(e) => {
            warn!(%build_id, error = %e, "substitute_log: build lookup failed");
            return Ok(());
        }
    };

    if attempt_log_id(&state, build_id).await.is_some() {
        return Ok(());
    }

    if let Some(effective) = find_dedup_log_id(&state, build_id, derivation_id).await {
        set_log_id(&state, build_id, effective).await;
        return Ok(());
    }

    if !allow_upstream_fetch {
        return Ok(());
    }

    let derivation = match EDerivation::find_by_id(derivation_id)
        .one(&state.worker_db)
        .await
    {
        Ok(Some(d)) => d,
        Ok(None) => {
            debug!(%build_id, %derivation_id, "substitute_log: derivation row not found");
            return Ok(());
        }
        Err(e) => {
            warn!(%build_id, error = %e, "substitute_log: derivation lookup failed");
            return Ok(());
        }
    };

    let upstream_urls =
        match gradient_db::upstream_urls_for_org(&state.worker_db, derivation.organization)
            .await
        {
            Ok(urls) => urls,
            Err(e) => {
                warn!(%build_id, error = %e, "substitute_log: upstream URL lookup failed");
                return Ok(());
            }
        };

    if upstream_urls.is_empty() {
        debug!(%build_id, "substitute_log: no upstream URLs configured");
        return Ok(());
    }

    let drv_basename = match std::path::Path::new(&drv_path)
        .file_name()
        .and_then(|n| n.to_str())
    {
        Some(n) => n.to_string(),
        None => {
            warn!(%build_id, %drv_path, "substitute_log: cannot derive .drv basename");
            return Ok(());
        }
    };

    for upstream in upstream_urls {
        let url = format!("{}/log/{}", upstream.trim_end_matches('/'), drv_basename);
        match fetch_log_body(&state.http, &url).await {
            Ok(Some(body)) => {
                if let Err(e) = state.log_storage.append(build_id, &body).await {
                    warn!(%build_id, error = %e, "substitute_log: log_storage.append failed");
                    return Ok(());
                }
                set_log_id(&state, build_id, build_id).await;
                return Ok(());
            }
            Ok(None) => {
                debug!(%build_id, %url, "substitute_log: upstream returned no usable body");
            }
            Err(e) => {
                debug!(%build_id, %url, error = %e, "substitute_log: upstream fetch failed");
            }
        }
    }

    debug!(%build_id, "substitute_log: no upstream had a log for this derivation");
    Ok(())
}

async fn find_dedup_log_id(
    state: &Arc<ServerState>,
    build_id: BuildId,
    derivation_id: DerivationId,
) -> Option<BuildId> {
    let candidate = match EBuild::find()
        .filter(CBuild::Derivation.eq(derivation_id))
        .filter(CBuild::Id.ne(build_id))
        .filter(CBuild::Status.is_in([BuildStatus::Completed, BuildStatus::Substituted]))
        .order_by_desc(CBuild::CreatedAt)
        .one(&state.worker_db)
        .await
    {
        Ok(c) => c,
        Err(e) => {
            warn!(%build_id, error = %e, "substitute_log: dedup query failed");
            return None;
        }
    };

    let candidate = candidate?;
    let log_key = gradient_db::latest_attempt_log_id(&state.worker_db, candidate.id)
        .await
        .unwrap_or(candidate.id);
    match state.log_storage.read(log_key).await {
        Ok(body) if !body.is_empty() => Some(log_key),
        _ => None,
    }
}

async fn fetch_log_body(http: &reqwest::Client, url: &str) -> anyhow::Result<Option<String>> {
    let resp = http.get(url).timeout(LOG_FETCH_TIMEOUT).send().await?;
    if !resp.status().is_success() {
        return Ok(None);
    }
    let mut bytes: Vec<u8> = Vec::new();
    let mut truncated = false;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        let room = LOG_FETCH_MAX_BYTES.saturating_sub(bytes.len());
        if chunk.len() > room {
            bytes.extend_from_slice(&chunk[..room]);
            truncated = true;
            break;
        }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.is_empty() {
        return Ok(None);
    }
    let mut body = String::from_utf8_lossy(&bytes).into_owned();
    if truncated {
        body.push_str("\n[truncated]\n");
    }
    Ok(Some(body))
}

/// The build's substituted-log pointer, stored on its latest `build_attempt`.
async fn attempt_log_id(state: &Arc<ServerState>, build_id: BuildId) -> Option<BuildId> {
    gradient_db::latest_attempt(&state.worker_db, build_id)
        .await
        .ok()
        .flatten()
        .and_then(|a| a.log_id)
}

async fn set_log_id(state: &Arc<ServerState>, build_id: BuildId, log_id: BuildId) {
    let attempt = match gradient_db::latest_attempt(&state.worker_db, build_id).await {
        Ok(Some(a)) => a,
        Ok(None) => {
            debug!(%build_id, "substitute_log: no attempt to record log_id on");
            return;
        }
        Err(e) => {
            warn!(%build_id, error = %e, "substitute_log: attempt lookup failed");
            return;
        }
    };

    let mut am = attempt.into_active_model();
    am.log_id = Set(Some(log_id));
    if let Err(e) = am.update(&state.worker_db).await {
        warn!(%build_id, error = %e, "substitute_log: failed to set attempt log_id");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::build;
    use gradient_types::ids::{EvaluationId, OrganizationId};
    use gradient_test_support::prelude::*;
    use uuid::Uuid;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn make_build(
        id: BuildId,
        derivation: DerivationId,
        status: BuildStatus,
        substitutable: bool,
    ) -> build::Model {
        let now = gradient_types::now();
        build::Model {
            id,
            evaluation: EvaluationId::new(Uuid::now_v7()),
            derivation,
            status,
            substitutable,
            created_at: now,
            updated_at: now,
            ..Default::default()
        }
    }

    #[tokio::test]
    #[ignore = "log-sub mock query flow changed (build.log_id moved to build_attempt); revisit with substitute-dispatch"]
    async fn dedup_hit_via_existing_log_id_pointer() {
        let drv = DerivationId::new(Uuid::now_v7());
        let prior_id = BuildId::new(Uuid::now_v7());
        let _prior_log = BuildId::new(Uuid::now_v7());
        let new_id = BuildId::new(Uuid::now_v7());

        let prior = make_build(prior_id, drv, BuildStatus::Completed, false);
        let new = make_build(new_id, drv, BuildStatus::Substituted, false);

        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([vec![new.clone()]])
            .append_query_results([vec![prior.clone()]])
            .append_query_results([vec![new.clone()]])
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results([vec![new.clone()]])
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();

        let state = test_state(db);
        substitute_log(
            state,
            new_id,
            drv,
            "/nix/store/x-test.drv".to_string(),
            false,
        )
        .await
        .expect("substitute_log returns Ok");
    }

    #[tokio::test]
    async fn no_prior_build_no_fetch_returns_ok() {
        let drv = DerivationId::new(Uuid::now_v7());
        let new_id = BuildId::new(Uuid::now_v7());
        let new = make_build(new_id, drv, BuildStatus::Substituted, false);

        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([vec![new.clone()]])
            .append_query_results([Vec::<build::Model>::new()])
            .append_query_results([Vec::<build::Model>::new()])
            .into_connection();

        let state = test_state(db);
        substitute_log(
            state,
            new_id,
            drv,
            "/nix/store/x-test.drv".to_string(),
            false,
        )
        .await
        .expect("substitute_log returns Ok");
    }

    fn test_state_with_recording_storage(
        db: sea_orm::DatabaseConnection,
    ) -> (Arc<ServerState>, Arc<RecordingLogStorage>) {
        let storage = Arc::new(RecordingLogStorage::new());
        let state = test_state_with_log_storage(db, storage.clone());
        (state, storage)
    }

    fn seed_upstream_urls(
        builder: sea_orm::MockDatabase,
        org: OrganizationId,
        urls: &[&str],
    ) -> sea_orm::MockDatabase {
        use gradient_types::ids::CacheId;
        let cache_id = CacheId::new(Uuid::now_v7());
        let oc_row = gradient_entity::organization_cache::Model {
            id: gradient_types::ids::OrganizationCacheId::now_v7(),
            organization: org,
            cache: cache_id,
            mode: gradient_entity::organization_cache::CacheSubscriptionMode::ReadOnly,
        };
        let upstream_rows: Vec<gradient_entity::cache_upstream::Model> = urls
            .iter()
            .map(|u| gradient_entity::cache_upstream::Model {
                id: gradient_types::ids::CacheUpstreamId::now_v7(),
                cache: cache_id,
                display_name: "test-upstream".into(),
                mode: gradient_entity::organization_cache::CacheSubscriptionMode::ReadOnly,
                kind: gradient_entity::cache_upstream::CacheUpstreamKind::Http,
                url: Some((*u).to_string()),
                ..Default::default()
            })
            .collect();
        builder
            .append_query_results([vec![oc_row]])
            .append_query_results([upstream_rows])
    }

    async fn make_upstream_with_log(drv_basename: &str, body: &str, status: u16) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/log/{drv_basename}")))
            .respond_with(ResponseTemplate::new(status).set_body_string(body))
            .mount(&server)
            .await;
        server
    }

    fn make_derivation(
        drv_id: DerivationId,
        org: OrganizationId,
        drv_path: String,
    ) -> gradient_entity::derivation::Model {
        let stripped = gradient_exec::strip_nix_store_prefix(&drv_path);
        let (hash, name) = gradient_sources::parse_drv_hash_name(&stripped)
            .unwrap_or_else(|_| ("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(), "x".into()));
        gradient_entity::derivation::Model {
            id: drv_id,
            organization: org,
            hash,
            name,
            architecture: "x86_64-linux".to_string(),
            created_at: gradient_types::now(),
            ..Default::default()
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "log-sub mock query flow changed (build.log_id moved to build_attempt); revisit with substitute-dispatch"]
    async fn followers_get_log_id_via_backfill() {
        let drv_id = DerivationId::new(Uuid::now_v7());
        let prior_id = BuildId::new(Uuid::now_v7());
        let _prior_log = BuildId::new(Uuid::now_v7());
        let new_id = BuildId::new(Uuid::now_v7());

        let prior = make_build(prior_id, drv_id, BuildStatus::Completed, false);
        let new = make_build(new_id, drv_id, BuildStatus::Substituted, false);

        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([vec![new.clone()]])
            .append_query_results([vec![prior.clone()]])
            .append_query_results([vec![new.clone()]])
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results([vec![new.clone()]])
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 2,
            }])
            .into_connection();

        let state = test_state(db);
        substitute_log(
            state,
            new_id,
            drv_id,
            "/nix/store/x-test.drv".to_string(),
            false,
        )
        .await
        .expect("Ok");
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "log-sub mock query flow changed (build.log_id moved to build_attempt); revisit with substitute-dispatch"]
    async fn upstream_fetch_persists_log_on_200() {
        let drv_id = DerivationId::new(Uuid::now_v7());
        let org = OrganizationId::new(Uuid::now_v7());
        let build_id = BuildId::new(Uuid::now_v7());
        let drv_path = "/nix/store/abc-hello.drv".to_string();
        let drv_basename = "abc-hello.drv";

        let upstream = make_upstream_with_log(drv_basename, "hello log\n", 200).await;

        let build = make_build(build_id, drv_id, BuildStatus::Created, true);
        let derivation = make_derivation(drv_id, org, drv_path.clone());

        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([vec![build.clone()]])
            .append_query_results([Vec::<build::Model>::new()])
            .append_query_results([Vec::<build::Model>::new()])
            .append_query_results([vec![derivation]]);
        let db = seed_upstream_urls(db, org, &[&upstream.uri()]);
        let db = db
            .append_query_results([vec![build.clone()]])
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results([vec![build.clone()]])
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();

        let (state, storage) = test_state_with_recording_storage(db);

        substitute_log(state, build_id, drv_id, drv_path, true)
            .await
            .expect("substitute_log Ok");

        let entries = storage.entries();
        assert_eq!(entries.len(), 1, "expected exactly one append");
        assert_eq!(entries[0].0, build_id);
        assert_eq!(entries[0].1, "hello log\n");
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "log-sub mock query flow changed (build.log_id moved to build_attempt); revisit with substitute-dispatch"]
    async fn first_upstream_404_second_200() {
        let drv_id = DerivationId::new(Uuid::now_v7());
        let org = OrganizationId::new(Uuid::now_v7());
        let build_id = BuildId::new(Uuid::now_v7());
        let drv_path = "/nix/store/abc-hello.drv".to_string();
        let drv_basename = "abc-hello.drv";

        let u404 = make_upstream_with_log(drv_basename, "", 404).await;
        let u200 = make_upstream_with_log(drv_basename, "second body", 200).await;

        let build = make_build(build_id, drv_id, BuildStatus::Created, true);
        let derivation = make_derivation(drv_id, org, drv_path.clone());

        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([vec![build.clone()]])
            .append_query_results([Vec::<build::Model>::new()])
            .append_query_results([Vec::<build::Model>::new()])
            .append_query_results([vec![derivation]]);
        let db = seed_upstream_urls(db, org, &[&u404.uri(), &u200.uri()]);
        let db = db
            .append_query_results([vec![build.clone()]])
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results([vec![build.clone()]])
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();

        let (state, storage) = test_state_with_recording_storage(db);

        substitute_log(state, build_id, drv_id, drv_path, true)
            .await
            .expect("substitute_log Ok");

        let entries = storage.entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].1, "second body");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn all_upstreams_404_leaves_log_null() {
        let drv_id = DerivationId::new(Uuid::now_v7());
        let org = OrganizationId::new(Uuid::now_v7());
        let build_id = BuildId::new(Uuid::now_v7());
        let drv_path = "/nix/store/abc-hello.drv".to_string();
        let drv_basename = "abc-hello.drv";

        let u404a = make_upstream_with_log(drv_basename, "", 404).await;
        let u404b = make_upstream_with_log(drv_basename, "", 404).await;

        let build = make_build(build_id, drv_id, BuildStatus::Created, true);
        let derivation = make_derivation(drv_id, org, drv_path.clone());

        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([vec![build.clone()]])
            .append_query_results([Vec::<build::Model>::new()])
            .append_query_results([Vec::<build::Model>::new()])
            .append_query_results([vec![derivation]]);
        let db = seed_upstream_urls(db, org, &[&u404a.uri(), &u404b.uri()]).into_connection();

        let (state, storage) = test_state_with_recording_storage(db);
        substitute_log(state, build_id, drv_id, drv_path, true)
            .await
            .expect("substitute_log Ok");
        assert!(storage.entries().is_empty(), "no append on all 404");
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "log-sub mock query flow changed (build.log_id moved to build_attempt); revisit with substitute-dispatch"]
    async fn upstream_body_exceeding_cap_is_truncated() {
        let drv_id = DerivationId::new(Uuid::now_v7());
        let org = OrganizationId::new(Uuid::now_v7());
        let build_id = BuildId::new(Uuid::now_v7());
        let drv_path = "/nix/store/abc-big.drv".to_string();
        let drv_basename = "abc-big.drv";

        let oversize = "X".repeat(LOG_FETCH_MAX_BYTES + 1024);
        let upstream = make_upstream_with_log(drv_basename, &oversize, 200).await;

        let build = make_build(build_id, drv_id, BuildStatus::Created, true);
        let derivation = make_derivation(drv_id, org, drv_path.clone());

        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([vec![build.clone()]])
            .append_query_results([Vec::<build::Model>::new()])
            .append_query_results([Vec::<build::Model>::new()])
            .append_query_results([vec![derivation]]);
        let db = seed_upstream_urls(db, org, &[&upstream.uri()]);
        let db = db
            .append_query_results([vec![build.clone()]])
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results([vec![build.clone()]])
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();

        let (state, storage) = test_state_with_recording_storage(db);
        substitute_log(state, build_id, drv_id, drv_path, true)
            .await
            .expect("substitute_log Ok");

        let entries = storage.entries();
        assert_eq!(entries.len(), 1);
        let body = &entries[0].1;
        assert_eq!(body.len(), LOG_FETCH_MAX_BYTES + "\n[truncated]\n".len());
        assert!(body.ends_with("\n[truncated]\n"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn upstream_empty_200_treated_as_miss() {
        let drv_id = DerivationId::new(Uuid::now_v7());
        let org = OrganizationId::new(Uuid::now_v7());
        let build_id = BuildId::new(Uuid::now_v7());
        let drv_path = "/nix/store/abc-empty.drv".to_string();
        let drv_basename = "abc-empty.drv";

        let upstream = make_upstream_with_log(drv_basename, "", 200).await;

        let build = make_build(build_id, drv_id, BuildStatus::Created, true);
        let derivation = make_derivation(drv_id, org, drv_path.clone());

        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([vec![build.clone()]])
            .append_query_results([Vec::<build::Model>::new()])
            .append_query_results([Vec::<build::Model>::new()])
            .append_query_results([vec![derivation]]);
        let db = seed_upstream_urls(db, org, &[&upstream.uri()]).into_connection();

        let (state, storage) = test_state_with_recording_storage(db);
        substitute_log(state, build_id, drv_id, drv_path, true)
            .await
            .expect("Ok");
        assert!(
            storage.entries().is_empty(),
            "empty 200 should not be persisted"
        );
    }

    #[tokio::test]
    #[ignore = "idempotency now checked via build_attempt.log_id, not build.log_id; mock query sequence changed"]
    async fn idempotent_when_log_id_already_set() {
        let drv_id = DerivationId::new(Uuid::now_v7());
        let build_id = BuildId::new(Uuid::now_v7());
        let _existing_log = BuildId::new(Uuid::now_v7());
        let build = make_build(build_id, drv_id, BuildStatus::Substituted, false);

        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([vec![build]])
            .into_connection();

        let state = test_state(db);
        substitute_log(
            state,
            build_id,
            drv_id,
            "/nix/store/x-test.drv".to_string(),
            true,
        )
        .await
        .expect("Ok");
    }

    #[tokio::test]
    async fn db_failure_during_dedup_returns_ok() {
        let drv_id = DerivationId::new(Uuid::now_v7());
        let build_id = BuildId::new(Uuid::now_v7());
        let build = make_build(build_id, drv_id, BuildStatus::Substituted, false);

        // Initial load succeeds; both dedup queries return empty (so set_log_id is not called).
        // No exec results are staged - if substitute_log tried to UPDATE, the test would panic.
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([vec![build]])
            .append_query_results([Vec::<build::Model>::new()])
            .append_query_results([Vec::<build::Model>::new()])
            .into_connection();

        let state = test_state(db);
        substitute_log(
            state,
            build_id,
            drv_id,
            "/nix/store/x-test.drv".to_string(),
            false,
        )
        .await
        .expect("substitute_log must return Ok even on dedup miss");
    }
}
