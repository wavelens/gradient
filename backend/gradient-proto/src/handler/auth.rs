/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_core::ServerState;
use gradient_types::ids::ProjectId;
use std::collections::HashSet;
use tracing::warn;
use uuid::Uuid;

use crate::messages::{FailedPeer, GradientCapabilities};

/// Demotes any authorized peer that is a project with zero
/// `project_cache` rows. Cache and proxy peer UUIDs (anything not present
/// in the `project` table) are passed through unchanged.
pub(super) async fn filter_project_peers_without_cache(
    state: &ServerState,
    authorized: Vec<String>,
) -> (Vec<String>, Vec<FailedPeer>) {
    use gradient_entity::project::{Column as OCol, Entity as EProject};
    use gradient_entity::project_cache::{Column as OCCol, Entity as EProjectCache};
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

    let mut authorized_out: Vec<String> = Vec::new();
    let mut uuid_peers: Vec<(String, ProjectId)> = Vec::new();
    for s in authorized {
        match Uuid::parse_str(&s) {
            Ok(u) => uuid_peers.push((s, ProjectId::new(u))),
            Err(_) => authorized_out.push(s),
        }
    }

    if uuid_peers.is_empty() {
        return (authorized_out, Vec::new());
    }

    let uuid_set: Vec<ProjectId> = uuid_peers.iter().map(|(_, u)| *u).collect();

    let project_ids: HashSet<ProjectId> = match EProject::find()
        .filter(OCol::Id.is_in(uuid_set.clone()))
        .all(&state.worker_db)
        .await
    {
        Ok(rows) => rows.into_iter().map(|r| r.id).collect(),
        Err(e) => {
            warn!(error = %e, "failed to look up projects for peer filter");
            for (s, _) in uuid_peers {
                authorized_out.push(s);
            }
            return (authorized_out, Vec::new());
        }
    };

    let projects_with_cache: HashSet<ProjectId> = if project_ids.is_empty() {
        HashSet::new()
    } else {
        match EProjectCache::find()
            .filter(OCCol::Project.is_in(project_ids.iter().copied().collect::<Vec<_>>()))
            .all(&state.worker_db)
            .await
        {
            Ok(rows) => rows.into_iter().map(|r| r.project).collect(),
            Err(e) => {
                warn!(error = %e, "failed to look up project_cache rows");
                for (s, _) in uuid_peers {
                    authorized_out.push(s);
                }
                return (authorized_out, Vec::new());
            }
        }
    };

    let mut demoted: Vec<FailedPeer> = Vec::new();
    for (s, u) in uuid_peers {
        if project_ids.contains(&u) && !projects_with_cache.contains(&u) {
            demoted.push(FailedPeer {
                peer_id: s,
                reason: "project has no cache subscribed".into(),
            });
        } else {
            authorized_out.push(s);
        }
    }

    (authorized_out, demoted)
}

/// Returns `(peer_id, token_hash)` pairs for all **active** peers that registered this worker.
pub(super) async fn lookup_registered_peers(
    state: &ServerState,
    worker_id: &str,
) -> Vec<(String, String)> {
    use gradient_entity::worker_registration::{Column, Entity};
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

    match Entity::find()
        .filter(Column::WorkerId.eq(worker_id))
        .filter(Column::Active.eq(true))
        .all(&state.worker_db)
        .await
    {
        Ok(rows) => rows
            .into_iter()
            .map(|r| (r.peer_id.to_string(), r.token_hash))
            .collect(),
        Err(e) => {
            warn!(error = %e, %worker_id, "failed to look up registered peers");
            vec![]
        }
    }
}

/// Challenge data for a connecting base worker.
pub(super) struct BaseWorkerChallenge {
    /// `(peer_id, token_hash)` pairs to send in the AuthChallenge.
    pub challenge: Vec<(String, String)>,
    /// When set, a successful auth of this single identity expands to
    /// `enabled_projects`; otherwise `challenge` already lists the enabled projects.
    pub authorize_against: Option<String>,
    /// Project UUID strings that opted into this base worker.
    pub enabled_projects: Vec<String>,
}

/// Returns base-worker challenge data when `worker_id` is an enabled base
/// worker, else `None`.
pub(super) async fn lookup_base_worker_challenge(
    state: &ServerState,
    worker_id: &str,
) -> Option<BaseWorkerChallenge> {
    let bw =
        gradient_db::base_workers::enabled_base_worker_by_worker_id(&state.worker_db, worker_id)
            .await
            .ok()
            .flatten()?;

    let enabled_projects: Vec<String> =
        gradient_db::base_workers::projects_enabling_base_worker(&state.worker_db, bw.id)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|o| o.to_string())
            .collect();

    let challenge = match &bw.authorize_against {
        Some(uuid) => vec![(uuid.to_string(), bw.token_hash.clone())],
        None => enabled_projects
            .iter()
            .map(|o| (o.clone(), bw.token_hash.clone()))
            .collect(),
    };

    Some(BaseWorkerChallenge {
        challenge,
        authorize_against: bw.authorize_against.map(|u| u.to_string()),
        enabled_projects,
    })
}

/// Applies a base worker's authorize_against expansion: in single-identity mode,
/// a successful auth of the identity expands to the enabled-project set; otherwise the
/// token-authorized set is returned unchanged.
pub(super) fn expand_base_authorized(
    base: &Option<BaseWorkerChallenge>,
    token_authorized: Vec<String>,
) -> Vec<String> {
    if let Some(b) = base
        && let Some(identity) = &b.authorize_against
    {
        return if token_authorized.iter().any(|p| p == identity) {
            b.enabled_projects.clone()
        } else {
            Vec::new()
        };
    }

    token_authorized
}

/// Per-registration capability gate aggregated across all **active**
/// registrations for a worker. A capability is enabled iff every active
/// registration enables it (AND across peers). Used to clamp the
/// worker-advertised capability set at handshake.
#[derive(Clone, Copy, Debug)]
pub(super) struct EnabledCapsAggregate {
    pub enable_fetch: bool,
    pub enable_eval: bool,
    pub enable_build: bool,
}

impl EnabledCapsAggregate {
    /// All-enabled default: used when there are no active registrations
    /// (so discovery-mode workers are not clamped).
    pub fn all() -> Self {
        Self {
            enable_fetch: true,
            enable_eval: true,
            enable_build: true,
        }
    }
}

pub(super) async fn aggregate_enabled_caps(
    state: &ServerState,
    worker_id: &str,
) -> EnabledCapsAggregate {
    use gradient_entity::worker_registration::{Column, Entity};
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

    let rows = match Entity::find()
        .filter(Column::WorkerId.eq(worker_id))
        .filter(Column::Active.eq(true))
        .all(&state.worker_db)
        .await
    {
        Ok(rows) => rows,
        Err(e) => {
            warn!(error = %e, %worker_id, "failed to aggregate enabled caps");
            return EnabledCapsAggregate::all();
        }
    };

    if rows.is_empty() {
        if let Ok(Some(bw)) =
            gradient_db::base_workers::enabled_base_worker_by_worker_id(&state.worker_db, worker_id)
                .await
        {
            return EnabledCapsAggregate {
                enable_fetch: bw.enable_fetch,
                enable_eval: bw.enable_eval,
                enable_build: bw.enable_build,
            };
        }

        return EnabledCapsAggregate::all();
    }

    EnabledCapsAggregate {
        enable_fetch: rows.iter().all(|r| r.enable_fetch),
        enable_eval: rows.iter().all(|r| r.enable_eval),
        enable_build: rows.iter().all(|r| r.enable_build),
    }
}

/// Returns `true` if *any* `worker_registration` row exists for this worker,
/// regardless of the `active` flag.  Used to distinguish "no registrations at
/// all" (open/discoverable mode) from "all registrations deactivated".
pub(super) async fn has_any_registrations(state: &ServerState, worker_id: &str) -> bool {
    use gradient_entity::worker_registration::{Column, Entity};
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

    match Entity::find()
        .filter(Column::WorkerId.eq(worker_id))
        .one(&state.worker_db)
        .await
    {
        Ok(row) => row.is_some(),
        Err(e) => {
            warn!(error = %e, %worker_id, "failed to check worker registrations");
            false
        }
    }
}

/// Verifies `token` against a stored `token_hash` in constant time.
///
/// Accepts two storage formats:
/// - PHC strings (e.g. `$argon2id$...`) - verified via `password_auth`,
///   which is constant-time and salted/KDF-hardened. This is what new
///   registrations write.
/// - Lowercase hex SHA-256 - legacy format from older registrations,
///   compared in constant time via `subtle::ConstantTimeEq`. New rows
///   are never written in this format.
pub(super) fn verify_token(token: &str, token_hash: &str) -> bool {
    if token_hash.starts_with('$') {
        password_auth::verify_password(token, token_hash).is_ok()
    } else {
        use sha2::{Digest, Sha256};
        use subtle::ConstantTimeEq;
        let digest = hex::encode(Sha256::digest(token.as_bytes()));
        digest.as_bytes().ct_eq(token_hash.as_bytes()).into()
    }
}

/// Validates `auth_tokens` (worker-supplied `(peer_id, plaintext_token)`) against
/// `registered_peers` (`(peer_id, stored_token_hash)`).
///
/// Returns `(authorized_peers, failed_peers)`.
pub(super) fn validate_tokens(
    registered_peers: &[(String, String)],
    auth_tokens: &[(String, String)],
) -> (Vec<String>, Vec<FailedPeer>) {
    let mut authorized = Vec::new();
    let mut failed = Vec::new();

    for (peer_id, token_hash) in registered_peers {
        match auth_tokens.iter().find(|(pid, _)| pid == peer_id) {
            Some((_, token)) => {
                if verify_token(token, token_hash) {
                    authorized.push(peer_id.clone());
                } else {
                    failed.push(FailedPeer {
                        peer_id: peer_id.clone(),
                        reason: "invalid token".into(),
                    });
                }
            }
            None => {
                failed.push(FailedPeer {
                    peer_id: peer_id.clone(),
                    reason: "no token provided".into(),
                });
            }
        }
    }

    (authorized, failed)
}

pub(super) fn negotiate_capabilities(
    state: &ServerState,
    client: GradientCapabilities,
    enabled: EnabledCapsAggregate,
) -> GradientCapabilities {
    GradientCapabilities {
        core: true,
        cache: true,
        federate: client.federate && state.config.proto.federate_proto,
        fetch: client.fetch && enabled.enable_fetch,
        eval: client.eval && enabled.enable_eval,
        build: client.build && enabled.enable_build,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn sha256_hex(s: &str) -> String {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(s.as_bytes()))
    }

    fn argon2(s: &str) -> String {
        password_auth::generate_hash(s)
    }

    fn all_caps(val: bool) -> GradientCapabilities {
        GradientCapabilities {
            core: val,
            cache: val,
            federate: val,
            fetch: val,
            eval: val,
            build: val,
        }
    }

    fn make_state(federate_proto: bool) -> ServerState {
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection();
        let mut state = Arc::try_unwrap(gradient_test_support::prelude::test_state(db)).unwrap();
        Arc::make_mut(&mut state.config).proto.federate_proto = federate_proto;
        state
    }

    // ── validate_tokens ──────────────────────────────────────────────────────

    #[test]
    fn validate_tokens_matching_hash_authorizes() {
        let token = "my-secret-token";
        let hash = sha256_hex(token);
        let registered = vec![("peer-a".to_string(), hash)];
        let auth = vec![("peer-a".to_string(), token.to_string())];
        let (authorized, failed) = validate_tokens(&registered, &auth);
        assert_eq!(authorized, vec!["peer-a"]);
        assert!(failed.is_empty());
    }

    #[test]
    fn validate_tokens_wrong_hash_fails() {
        let registered = vec![("peer-a".to_string(), sha256_hex("correct-token"))];
        let auth = vec![("peer-a".to_string(), "wrong-token".to_string())];
        let (authorized, failed) = validate_tokens(&registered, &auth);
        assert!(authorized.is_empty());
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].peer_id, "peer-a");
        assert!(failed[0].reason.contains("invalid token"));
    }

    #[test]
    fn validate_tokens_missing_token_fails() {
        let registered = vec![("peer-a".to_string(), sha256_hex("some-token"))];
        let (authorized, failed) = validate_tokens(&registered, &[]);
        assert!(authorized.is_empty());
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].peer_id, "peer-a");
        assert!(failed[0].reason.contains("no token provided"));
    }

    #[test]
    fn validate_tokens_mixed_results() {
        let tok_b = "token-b";
        let registered = vec![
            ("peer-a".to_string(), sha256_hex("token-a")),
            ("peer-b".to_string(), sha256_hex(tok_b)),
            ("peer-c".to_string(), sha256_hex("token-c")),
        ];
        let auth = vec![
            ("peer-a".to_string(), "token-a".to_string()), // correct
            ("peer-b".to_string(), "wrong".to_string()),   // wrong hash
                                                           // peer-c missing
        ];
        let (authorized, failed) = validate_tokens(&registered, &auth);
        assert_eq!(authorized, vec!["peer-a"]);
        assert_eq!(failed.len(), 2);
        let failed_ids: Vec<&str> = failed.iter().map(|f| f.peer_id.as_str()).collect();
        assert!(failed_ids.contains(&"peer-b"));
        assert!(failed_ids.contains(&"peer-c"));
    }

    #[test]
    fn validate_tokens_empty_inputs() {
        let (authorized, failed) = validate_tokens(&[], &[]);
        assert!(authorized.is_empty());
        assert!(failed.is_empty());
    }

    #[test]
    fn validate_tokens_extra_tokens_ignored() {
        let auth = vec![("unknown-peer".to_string(), "some-token".to_string())];
        let (authorized, failed) = validate_tokens(&[], &auth);
        assert!(authorized.is_empty());
        assert!(failed.is_empty());
    }

    #[test]
    fn validate_tokens_argon2_hash_authorizes() {
        let token = "argon2-token";
        let registered = vec![("peer-a".to_string(), argon2(token))];
        let auth = vec![("peer-a".to_string(), token.to_string())];
        let (authorized, failed) = validate_tokens(&registered, &auth);
        assert_eq!(authorized, vec!["peer-a"]);
        assert!(failed.is_empty());
    }

    #[test]
    fn validate_tokens_argon2_wrong_token_fails() {
        let registered = vec![("peer-a".to_string(), argon2("correct"))];
        let auth = vec![("peer-a".to_string(), "wrong".to_string())];
        let (authorized, failed) = validate_tokens(&registered, &auth);
        assert!(authorized.is_empty());
        assert_eq!(failed.len(), 1);
        assert!(failed[0].reason.contains("invalid token"));
    }

    #[test]
    fn verify_token_dispatches_on_format() {
        let token = "tok";
        // PHC string → argon2 path
        let phc = argon2(token);
        assert!(phc.starts_with('$'), "argon2 hash must start with $");
        assert!(verify_token(token, &phc));
        // Hex SHA-256 → legacy path
        assert!(verify_token(token, &sha256_hex(token)));
        // Wrong inputs both reject
        assert!(!verify_token("bad", &phc));
        assert!(!verify_token("bad", &sha256_hex(token)));
    }

    #[test]
    fn validate_tokens_duplicate_peer_first_wins() {
        let token = "correct";
        let registered = vec![("peer-a".to_string(), sha256_hex(token))];
        let auth = vec![
            ("peer-a".to_string(), token.to_string()),
            ("peer-a".to_string(), "wrong".to_string()),
        ];
        let (authorized, failed) = validate_tokens(&registered, &auth);
        assert_eq!(authorized, vec!["peer-a"]);
        assert!(failed.is_empty());
    }

    // ── negotiate_capabilities ───────────────────────────────────────────────

    #[test]
    fn negotiate_capabilities_core_always_true() {
        let state = make_state(false);
        let result = negotiate_capabilities(&state, all_caps(false), EnabledCapsAggregate::all());
        assert!(result.core);
    }

    #[test]
    fn negotiate_capabilities_cache_always_true() {
        let state = make_state(false);
        assert!(negotiate_capabilities(&state, all_caps(false), EnabledCapsAggregate::all()).cache);
        assert!(negotiate_capabilities(&state, all_caps(true), EnabledCapsAggregate::all()).cache);
    }

    #[test]
    fn negotiate_capabilities_federate_requires_both() {
        assert!(
            !negotiate_capabilities(
                &make_state(false),
                all_caps(true),
                EnabledCapsAggregate::all()
            )
            .federate
        );
        assert!(
            !negotiate_capabilities(
                &make_state(true),
                all_caps(false),
                EnabledCapsAggregate::all()
            )
            .federate
        );
        assert!(
            negotiate_capabilities(
                &make_state(true),
                all_caps(true),
                EnabledCapsAggregate::all()
            )
            .federate
        );
    }

    #[test]
    fn negotiate_capabilities_passthrough_fields() {
        let state = make_state(false);
        let client = GradientCapabilities {
            core: false,
            cache: false,
            federate: false,
            fetch: true,
            eval: true,
            build: true,
        };
        let result = negotiate_capabilities(&state, client, EnabledCapsAggregate::all());
        assert!(result.fetch);
        assert!(result.eval);
        assert!(result.build);
    }

    #[test]
    fn negotiate_capabilities_clamped_by_enabled_aggregate() {
        let state = make_state(false);
        let client = all_caps(true);
        let enabled = EnabledCapsAggregate {
            enable_fetch: false,
            enable_eval: true,
            enable_build: false,
        };
        let result = negotiate_capabilities(&state, client, enabled);
        assert!(!result.fetch);
        assert!(result.eval);
        assert!(!result.build);
    }

    #[test]
    fn negotiate_capabilities_all_false_client() {
        let state = make_state(false);
        let result = negotiate_capabilities(&state, all_caps(false), EnabledCapsAggregate::all());
        assert!(result.core);
        assert!(result.cache);
        assert!(!result.federate);
        assert!(!result.fetch);
        assert!(!result.eval);
        assert!(!result.build);
    }

    // ── base worker challenge ────────────────────────────────────────────────

    fn base_challenge(authorize_against: Option<&str>, enabled: &[&str]) -> BaseWorkerChallenge {
        BaseWorkerChallenge {
            challenge: enabled
                .iter()
                .map(|o| ((*o).into(), "hash".into()))
                .collect(),
            authorize_against: authorize_against.map(|s| s.into()),
            enabled_projects: enabled.iter().map(|o| (*o).into()).collect(),
        }
    }

    #[test]
    fn expand_base_authorized_identity_present_expands_to_enabled_projects() {
        let base = Some(base_challenge(Some("id-1"), &["project-1", "project-2"]));
        let out = expand_base_authorized(&base, vec!["id-1".into()]);
        assert_eq!(out, vec!["project-1".to_string(), "project-2".to_string()]);
    }

    #[test]
    fn expand_base_authorized_identity_absent_returns_empty() {
        let base = Some(base_challenge(Some("id-1"), &["project-1", "project-2"]));
        let out = expand_base_authorized(&base, vec!["other".into()]);
        assert!(out.is_empty());
    }

    #[test]
    fn expand_base_authorized_per_project_mode_passes_through_unchanged() {
        let base = Some(base_challenge(None, &["project-1", "project-2"]));
        let token_authorized = vec!["project-1".to_string()];
        let out = expand_base_authorized(&base, token_authorized.clone());
        assert_eq!(out, token_authorized);
    }

    #[test]
    fn expand_base_authorized_none_passes_through_unchanged() {
        let token_authorized = vec!["project-1".to_string(), "project-2".to_string()];
        let out = expand_base_authorized(&None, token_authorized.clone());
        assert_eq!(out, token_authorized);
    }

    // ── filter_project_peers_without_cache ───────────────────────────────────────

    use gradient_entity::project::Model as ProjectModel;
    use gradient_entity::project_cache::{CacheSubscriptionMode, Model as ProjectCacheModel};
    use sea_orm::{DatabaseBackend, MockDatabase};

    fn project_row(id: ProjectId) -> ProjectModel {
        ProjectModel {
            id,
            name: format!("o-{}", id),
            display_name: "test".into(),
            description: String::new(),
            public_key: String::new(),
            private_key: String::new(),
            public: false,
            hide_build_requests: false,
            created_by: gradient_types::ids::UserId::nil(),
            created_at: gradient_types::now(),
            managed: false,
        }
    }

    fn project_cache_row(
        project: ProjectId,
        cache: gradient_types::ids::CacheId,
    ) -> ProjectCacheModel {
        ProjectCacheModel {
            id: gradient_types::ids::ProjectCacheId::now_v7(),
            project,
            cache,
            mode: CacheSubscriptionMode::ReadWrite,
        }
    }

    fn state_with_db(db: sea_orm::DatabaseConnection) -> ServerState {
        Arc::try_unwrap(gradient_test_support::prelude::test_state(db)).unwrap()
    }

    #[tokio::test]
    async fn filter_project_peers_passes_through_project_with_cache() {
        let project = ProjectId::now_v7();
        let cache = gradient_types::ids::CacheId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![project_row(project)]])
            .append_query_results([vec![project_cache_row(project, cache)]])
            .into_connection();
        let state = state_with_db(db);
        let (authorized, demoted) =
            filter_project_peers_without_cache(&state, vec![project.to_string()]).await;
        assert_eq!(authorized, vec![project.to_string()]);
        assert!(demoted.is_empty());
    }

    #[tokio::test]
    async fn filter_project_peers_demotes_project_without_cache() {
        let project = ProjectId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![project_row(project)]])
            .append_query_results([Vec::<ProjectCacheModel>::new()])
            .into_connection();
        let state = state_with_db(db);
        let (authorized, demoted) =
            filter_project_peers_without_cache(&state, vec![project.to_string()]).await;
        assert!(authorized.is_empty());
        assert_eq!(demoted.len(), 1);
        assert_eq!(demoted[0].peer_id, project.to_string());
        assert!(
            demoted[0]
                .reason
                .contains("project has no cache subscribed")
        );
    }

    #[tokio::test]
    async fn filter_project_peers_passes_through_non_project_uuids() {
        let cache_peer = ProjectId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<ProjectModel>::new()])
            .into_connection();
        let state = state_with_db(db);
        let (authorized, demoted) =
            filter_project_peers_without_cache(&state, vec![cache_peer.to_string()]).await;
        assert_eq!(authorized, vec![cache_peer.to_string()]);
        assert!(demoted.is_empty());
    }

    #[tokio::test]
    async fn filter_project_peers_mixed() {
        let project_with = ProjectId::now_v7();
        let project_without = ProjectId::now_v7();
        let cache = gradient_types::ids::CacheId::now_v7();
        let cache_peer = ProjectId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![
                project_row(project_with),
                project_row(project_without),
            ]])
            .append_query_results([vec![project_cache_row(project_with, cache)]])
            .into_connection();
        let state = state_with_db(db);
        let (authorized, demoted) = filter_project_peers_without_cache(
            &state,
            vec![
                project_with.to_string(),
                project_without.to_string(),
                cache_peer.to_string(),
            ],
        )
        .await;
        assert!(authorized.contains(&project_with.to_string()));
        assert!(authorized.contains(&cache_peer.to_string()));
        assert_eq!(demoted.len(), 1);
        assert_eq!(demoted[0].peer_id, project_without.to_string());
    }

    #[tokio::test]
    async fn validate_then_filter_demotes_project_without_cache() {
        let token = "token-x";
        let project_with = ProjectId::now_v7();
        let project_without = ProjectId::now_v7();
        let registered = vec![
            (project_with.to_string(), sha256_hex(token)),
            (project_without.to_string(), sha256_hex(token)),
        ];
        let auth = vec![
            (project_with.to_string(), token.to_string()),
            (project_without.to_string(), token.to_string()),
        ];
        let (authorized, failed) = validate_tokens(&registered, &auth);
        assert_eq!(authorized.len(), 2);
        assert!(failed.is_empty());

        let cache = gradient_types::ids::CacheId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![
                project_row(project_with),
                project_row(project_without),
            ]])
            .append_query_results([vec![project_cache_row(project_with, cache)]])
            .into_connection();
        let state = state_with_db(db);

        let (authorized, demoted) = filter_project_peers_without_cache(&state, authorized).await;
        let mut failed = failed;
        failed.extend(demoted);

        assert_eq!(authorized, vec![project_with.to_string()]);
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].peer_id, project_without.to_string());
        assert!(failed[0].reason.contains("project has no cache subscribed"));
    }
}
