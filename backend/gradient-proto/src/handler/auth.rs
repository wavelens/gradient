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

use gradient_wire::auth::validate_tokens;
use gradient_wire::messages::{FailedPeer, GradientCapabilities};

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

pub(super) struct TeamWorkerChallenge {
    pub team: String,
    pub token_hash: String,
    pub granted_projects: Vec<String>,
}

pub(super) async fn lookup_team_worker_challenge(
    state: &ServerState,
    worker_id: &str,
) -> Option<TeamWorkerChallenge> {
    let worker = gradient_db::teams::workers::active_team_worker(&state.worker_db, worker_id)
        .await
        .ok()
        .flatten()?;
    let granted_projects =
        gradient_db::teams::workers::projects_granted_with_workers(&state.worker_db, worker.team)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|project| project.to_string())
            .collect();

    Some(TeamWorkerChallenge {
        team: worker.team.to_string(),
        token_hash: worker.token_hash,
        granted_projects,
    })
}

pub(super) async fn challenge_for(
    state: &ServerState,
    worker_id: &str,
) -> (Option<TeamWorkerChallenge>, Vec<(String, String)>) {
    match lookup_team_worker_challenge(state, worker_id).await {
        Some(team) => {
            let peers = vec![(team.team.clone(), team.token_hash.clone())];
            (Some(team), peers)
        }
        None => (None, lookup_registered_peers(state, worker_id).await),
    }
}

pub(super) fn expand_team_authorized(
    team: &Option<TeamWorkerChallenge>,
    token_authorized: Vec<String>,
) -> Vec<String> {
    match team {
        Some(team) if token_authorized.contains(&team.team) => team.granted_projects.clone(),
        Some(_) => Vec::new(),
        None => token_authorized,
    }
}

pub(super) struct Resolved {
    pub authorized: Vec<String>,
    pub failed: Vec<FailedPeer>,
    pub proven: bool,
    pub emptied_by_missing_cache: bool,
}

pub(super) async fn resolve_authorized(
    state: &ServerState,
    team: &Option<TeamWorkerChallenge>,
    registered: &[(String, String)],
    tokens: &[(String, String)],
) -> Resolved {
    let (token_authorized, mut failed) = validate_tokens(registered, tokens);
    let proven = !token_authorized.is_empty();
    let expanded = expand_team_authorized(team, token_authorized);
    let had_any = !expanded.is_empty();
    let (authorized, demoted) = filter_project_peers_without_cache(state, expanded).await;
    let emptied_by_missing_cache = authorized.is_empty() && had_any && !demoted.is_empty();
    failed.extend(demoted);

    Resolved {
        authorized,
        failed,
        proven,
        emptied_by_missing_cache,
    }
}

/// A capability is enabled only when every active registration is enabling it. The handshake is
/// clamping the advertised set with it.
#[derive(Clone, Copy, Debug)]
pub(super) struct EnabledCapsAggregate {
    pub enable_fetch: bool,
    pub enable_eval: bool,
    pub enable_build: bool,
}

impl EnabledCapsAggregate {
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
        if let Ok(Some(worker)) =
            gradient_db::teams::workers::active_team_worker(&state.worker_db, worker_id).await
        {
            return EnabledCapsAggregate {
                enable_fetch: worker.enable_fetch,
                enable_eval: worker.enable_eval,
                enable_build: worker.enable_build,
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

pub(super) fn negotiate_capabilities(
    state: &ServerState,
    client: GradientCapabilities,
    enabled: EnabledCapsAggregate,
) -> GradientCapabilities {
    GradientCapabilities {
        core: true,
        cache: true,
        federate: client.federate && state.config.proto.federate,
        fetch: client.fetch && enabled.enable_fetch,
        eval: client.eval && enabled.enable_eval,
        build: client.build && enabled.enable_build,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

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
        Arc::make_mut(&mut state.config).proto.federate = federate_proto;
        state
    }

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

    const TEAM: &str = "7e000000-0000-0000-0000-0000000000aa";
    const SHA256_OF_T1: &str = "628b49d96dcde97a430dd4f597705899e09a968f793491e4b704cae33a40dc02";

    fn team_challenge(granted: &[ProjectId]) -> Option<TeamWorkerChallenge> {
        Some(TeamWorkerChallenge {
            team: TEAM.into(),
            token_hash: SHA256_OF_T1.into(),
            granted_projects: granted.iter().map(ToString::to_string).collect(),
        })
    }

    #[test]
    fn a_team_token_expands_to_the_projects_granted_with_workers() {
        let (a, b) = (ProjectId::now_v7(), ProjectId::now_v7());
        let expanded = expand_team_authorized(&team_challenge(&[a, b]), vec![TEAM.into()]);
        assert_eq!(expanded, vec![a.to_string(), b.to_string()]);
    }

    #[test]
    fn a_team_worker_without_a_valid_team_token_gets_nothing() {
        let expanded = expand_team_authorized(&team_challenge(&[ProjectId::now_v7()]), vec![]);
        assert!(expanded.is_empty());
    }

    #[test]
    fn a_project_worker_keeps_the_projects_its_tokens_prove() {
        let project = ProjectId::now_v7().to_string();
        assert_eq!(
            expand_team_authorized(&None, vec![project.clone()]),
            vec![project]
        );
    }

    #[tokio::test]
    async fn a_team_without_projects_granted_with_workers_resolves_to_nothing() {
        let state = state_with_db(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let team = team_challenge(&[]);
        let registered = vec![(TEAM.to_string(), SHA256_OF_T1.to_string())];

        let resolved =
            resolve_authorized(&state, &team, &registered, &[(TEAM.into(), "t1".into())]).await;

        assert!(resolved.proven);
        assert!(resolved.authorized.is_empty());
        assert!(!resolved.emptied_by_missing_cache);
    }

    #[tokio::test]
    async fn granted_projects_without_a_cache_are_demoted() {
        let project = ProjectId::now_v7();
        let state = state_with_db(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![project_row(project)]])
                .append_query_results([Vec::<gradient_entity::project_cache::Model>::new()])
                .into_connection(),
        );
        let team = team_challenge(&[project]);
        let registered = vec![(TEAM.to_string(), SHA256_OF_T1.to_string())];

        let resolved =
            resolve_authorized(&state, &team, &registered, &[(TEAM.into(), "t1".into())]).await;

        assert!(resolved.authorized.is_empty());
        assert!(resolved.emptied_by_missing_cache);
    }
}
