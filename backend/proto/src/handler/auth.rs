/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_core::types::ServerState;
use std::collections::HashSet;
use tracing::warn;
use uuid::Uuid;

use crate::messages::{FailedPeer, GradientCapabilities};

/// Demotes any authorized peer that is an organization with zero
/// `organization_cache` rows. Cache and proxy peer UUIDs (anything not present
/// in the `organization` table) are passed through unchanged.
pub(super) async fn filter_org_peers_without_cache(
    state: &ServerState,
    authorized: Vec<String>,
) -> (Vec<String>, Vec<FailedPeer>) {
    use entity::organization::{Column as OCol, Entity as EOrg};
    use entity::organization_cache::{Column as OCCol, Entity as EOrgCache};
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

    let mut authorized_out: Vec<String> = Vec::new();
    let mut uuid_peers: Vec<(String, Uuid)> = Vec::new();
    for s in authorized {
        match Uuid::parse_str(&s) {
            Ok(u) => uuid_peers.push((s, u)),
            Err(_) => authorized_out.push(s),
        }
    }

    if uuid_peers.is_empty() {
        return (authorized_out, Vec::new());
    }

    let uuid_set: Vec<Uuid> = uuid_peers.iter().map(|(_, u)| *u).collect();

    let org_ids: HashSet<Uuid> = match EOrg::find()
        .filter(OCol::Id.is_in(uuid_set.clone()))
        .all(&state.db)
        .await
    {
        Ok(rows) => rows.into_iter().map(|r| r.id).collect(),
        Err(e) => {
            warn!(error = %e, "failed to look up organizations for peer filter");
            for (s, _) in uuid_peers {
                authorized_out.push(s);
            }
            return (authorized_out, Vec::new());
        }
    };

    let orgs_with_cache: HashSet<Uuid> = if org_ids.is_empty() {
        HashSet::new()
    } else {
        match EOrgCache::find()
            .filter(OCCol::Organization.is_in(org_ids.iter().copied().collect::<Vec<_>>()))
            .all(&state.db)
            .await
        {
            Ok(rows) => rows.into_iter().map(|r| r.organization).collect(),
            Err(e) => {
                warn!(error = %e, "failed to look up organization_cache rows");
                for (s, _) in uuid_peers {
                    authorized_out.push(s);
                }
                return (authorized_out, Vec::new());
            }
        }
    };

    let mut demoted: Vec<FailedPeer> = Vec::new();
    for (s, u) in uuid_peers {
        if org_ids.contains(&u) && !orgs_with_cache.contains(&u) {
            demoted.push(FailedPeer {
                peer_id: s,
                reason: "organization has no cache subscribed".into(),
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
    use entity::worker_registration::{Column, Entity};
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

    match Entity::find()
        .filter(Column::WorkerId.eq(worker_id))
        .filter(Column::Active.eq(true))
        .all(&state.db)
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

/// Returns `true` if *any* `worker_registration` row exists for this worker,
/// regardless of the `active` flag.  Used to distinguish "no registrations at
/// all" (open/discoverable mode) from "all registrations deactivated".
pub(super) async fn has_any_registrations(state: &ServerState, worker_id: &str) -> bool {
    use entity::worker_registration::{Column, Entity};
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

    match Entity::find()
        .filter(Column::WorkerId.eq(worker_id))
        .one(&state.db)
        .await
    {
        Ok(row) => row.is_some(),
        Err(e) => {
            warn!(error = %e, %worker_id, "failed to check worker registrations");
            false
        }
    }
}

/// Validates `auth_tokens` (worker-supplied `(peer_id, plaintext_token)`) against
/// `registered_peers` (`(peer_id, sha256_token_hash)`).
///
/// Returns `(authorized_peers, failed_peers)`.
pub(super) fn validate_tokens(
    registered_peers: &[(String, String)],
    auth_tokens: &[(String, String)],
) -> (Vec<String>, Vec<FailedPeer>) {
    use sha2::{Digest, Sha256};

    let mut authorized = Vec::new();
    let mut failed = Vec::new();

    for (peer_id, token_hash) in registered_peers {
        match auth_tokens.iter().find(|(pid, _)| pid == peer_id) {
            Some((_, token)) => {
                let digest = hex::encode(Sha256::digest(token.as_bytes()));
                if digest == *token_hash {
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
) -> GradientCapabilities {
    GradientCapabilities {
        core: true,
        cache: true,
        federate: client.federate && state.cli.federate_proto,
        fetch: client.fetch,
        eval: client.eval,
        build: client.build,
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
        let mut state = Arc::try_unwrap(test_support::prelude::test_state(db)).unwrap();
        state.cli.federate_proto = federate_proto;
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
        let result = negotiate_capabilities(&state, all_caps(false));
        assert!(result.core);
    }

    #[test]
    fn negotiate_capabilities_cache_always_true() {
        let state = make_state(false);
        assert!(negotiate_capabilities(&state, all_caps(false)).cache);
        assert!(negotiate_capabilities(&state, all_caps(true)).cache);
    }

    #[test]
    fn negotiate_capabilities_federate_requires_both() {
        assert!(!negotiate_capabilities(&make_state(false), all_caps(true)).federate);
        assert!(!negotiate_capabilities(&make_state(true), all_caps(false)).federate);
        assert!(negotiate_capabilities(&make_state(true), all_caps(true)).federate);
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
        let result = negotiate_capabilities(&state, client);
        assert!(result.fetch);
        assert!(result.eval);
        assert!(result.build);
    }

    #[test]
    fn negotiate_capabilities_all_false_client() {
        let state = make_state(false);
        let result = negotiate_capabilities(&state, all_caps(false));
        assert!(result.core);
        assert!(result.cache);
        assert!(!result.federate);
        assert!(!result.fetch);
        assert!(!result.eval);
        assert!(!result.build);
    }

    // ── filter_org_peers_without_cache ───────────────────────────────────────

    use entity::organization::Model as OrgModel;
    use entity::organization_cache::{CacheSubscriptionMode, Model as OrgCacheModel};
    use sea_orm::{DatabaseBackend, MockDatabase};
    use uuid::Uuid;

    fn org_row(id: Uuid) -> OrgModel {
        OrgModel {
            id,
            name: format!("o-{}", id),
            display_name: "test".into(),
            description: String::new(),
            public_key: String::new(),
            private_key: String::new(),
            public: false,
            created_by: Uuid::nil(),
            created_at: chrono::Utc::now().naive_utc(),
            managed: false,
            github_installation_id: None,
            github_app_enabled: false,
        }
    }

    fn org_cache_row(org: Uuid, cache: Uuid) -> OrgCacheModel {
        OrgCacheModel {
            id: Uuid::new_v4(),
            organization: org,
            cache,
            mode: CacheSubscriptionMode::ReadWrite,
        }
    }

    fn state_with_db(db: sea_orm::DatabaseConnection) -> ServerState {
        Arc::try_unwrap(test_support::prelude::test_state(db)).unwrap()
    }

    #[tokio::test]
    async fn filter_org_peers_passes_through_org_with_cache() {
        let org = Uuid::new_v4();
        let cache = Uuid::new_v4();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![org_row(org)]])
            .append_query_results([vec![org_cache_row(org, cache)]])
            .into_connection();
        let state = state_with_db(db);
        let (authorized, demoted) =
            filter_org_peers_without_cache(&state, vec![org.to_string()]).await;
        assert_eq!(authorized, vec![org.to_string()]);
        assert!(demoted.is_empty());
    }

    #[tokio::test]
    async fn filter_org_peers_demotes_org_without_cache() {
        let org = Uuid::new_v4();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![org_row(org)]])
            .append_query_results([Vec::<OrgCacheModel>::new()])
            .into_connection();
        let state = state_with_db(db);
        let (authorized, demoted) =
            filter_org_peers_without_cache(&state, vec![org.to_string()]).await;
        assert!(authorized.is_empty());
        assert_eq!(demoted.len(), 1);
        assert_eq!(demoted[0].peer_id, org.to_string());
        assert!(demoted[0]
            .reason
            .contains("organization has no cache subscribed"));
    }

    #[tokio::test]
    async fn filter_org_peers_passes_through_non_org_uuids() {
        let cache_peer = Uuid::new_v4();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<OrgModel>::new()])
            .into_connection();
        let state = state_with_db(db);
        let (authorized, demoted) =
            filter_org_peers_without_cache(&state, vec![cache_peer.to_string()]).await;
        assert_eq!(authorized, vec![cache_peer.to_string()]);
        assert!(demoted.is_empty());
    }

    #[tokio::test]
    async fn filter_org_peers_mixed() {
        let org_with = Uuid::new_v4();
        let org_without = Uuid::new_v4();
        let cache = Uuid::new_v4();
        let cache_peer = Uuid::new_v4();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![org_row(org_with), org_row(org_without)]])
            .append_query_results([vec![org_cache_row(org_with, cache)]])
            .into_connection();
        let state = state_with_db(db);
        let (authorized, demoted) = filter_org_peers_without_cache(
            &state,
            vec![
                org_with.to_string(),
                org_without.to_string(),
                cache_peer.to_string(),
            ],
        )
        .await;
        assert!(authorized.contains(&org_with.to_string()));
        assert!(authorized.contains(&cache_peer.to_string()));
        assert_eq!(demoted.len(), 1);
        assert_eq!(demoted[0].peer_id, org_without.to_string());
    }

    #[tokio::test]
    async fn validate_then_filter_demotes_org_without_cache() {
        let token = "token-x";
        let org_with = Uuid::new_v4();
        let org_without = Uuid::new_v4();
        let registered = vec![
            (org_with.to_string(), sha256_hex(token)),
            (org_without.to_string(), sha256_hex(token)),
        ];
        let auth = vec![
            (org_with.to_string(), token.to_string()),
            (org_without.to_string(), token.to_string()),
        ];
        let (authorized, failed) = validate_tokens(&registered, &auth);
        assert_eq!(authorized.len(), 2);
        assert!(failed.is_empty());

        let cache = Uuid::new_v4();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![org_row(org_with), org_row(org_without)]])
            .append_query_results([vec![org_cache_row(org_with, cache)]])
            .into_connection();
        let state = state_with_db(db);

        let (authorized, demoted) = filter_org_peers_without_cache(&state, authorized).await;
        let mut failed = failed;
        failed.extend(demoted);

        assert_eq!(authorized, vec![org_with.to_string()]);
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].peer_id, org_without.to_string());
        assert!(failed[0]
            .reason
            .contains("organization has no cache subscribed"));
    }
}
