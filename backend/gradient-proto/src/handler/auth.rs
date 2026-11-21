/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Context as _;
use gradient_db::WorkerDb;
use gradient_entity::{team_worker, worker_registration};
use gradient_scheduler::connection_failures::ConnectionDirection;
use gradient_types::ids::ProjectId;
use gradient_types::{RuntimeConfig, SecretString};
use sea_orm::sea_query::SimpleExpr;
use sea_orm::{ColumnTrait, DbErr, EntityTrait, QueryFilter};
use std::collections::HashSet;
use std::sync::{Arc, LazyLock};
use tokio::sync::Semaphore;
use uuid::Uuid;

use gradient_wire::auth::validate_tokens;
use gradient_wire::messages::{FailedPeer, GradientCapabilities};

pub(super) async fn filter_project_peers_without_cache(
    db: &WorkerDb,
    authorized: Vec<String>,
) -> Result<(Vec<String>, Vec<FailedPeer>), DbErr> {
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
        return Ok((authorized_out, Vec::new()));
    }

    let uuid_set: Vec<ProjectId> = uuid_peers.iter().map(|(_, u)| *u).collect();

    let project_ids: HashSet<ProjectId> =
        gradient_db::fetch_in_chunks(&uuid_set, |chunk| async move {
            EProject::find().filter(OCol::Id.is_in(chunk)).all(db).await
        })
        .await?
        .into_iter()
        .map(|r| r.id)
        .collect();

    let projects_with_cache: HashSet<ProjectId> = if project_ids.is_empty() {
        HashSet::new()
    } else {
        EProjectCache::find()
            .filter(OCCol::Project.is_in(project_ids.iter().copied().collect::<Vec<_>>()))
            .all(db)
            .await?
            .into_iter()
            .map(|r| r.project)
            .collect()
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

    Ok((authorized_out, demoted))
}

fn dial_url_sql_condition_for_connection_direction(
    url: impl ColumnTrait,
    direction: ConnectionDirection,
) -> SimpleExpr {
    match direction {
        ConnectionDirection::Inbound => url.is_null(),
        ConnectionDirection::Outbound => url.is_not_null(),
    }
}

async fn load_active_team_worker_by_worker_id_and_connection_direction(
    db: &WorkerDb,
    worker_id: &str,
    direction: ConnectionDirection,
) -> Result<Option<team_worker::Model>, DbErr> {
    use team_worker::{Column, Entity};

    Entity::find()
        .filter(Column::WorkerId.eq(worker_id))
        .filter(Column::Active.eq(true))
        .filter(dial_url_sql_condition_for_connection_direction(
            Column::Url,
            direction,
        ))
        .one(db)
        .await
}

async fn lookup_registered_peers(
    db: &WorkerDb,
    worker_id: &str,
) -> Result<Vec<(String, String)>, DbErr> {
    use worker_registration::{Column, Entity};

    Ok(Entity::find()
        .filter(Column::WorkerId.eq(worker_id))
        .filter(Column::Active.eq(true))
        .filter(dial_url_sql_condition_for_connection_direction(
            Column::Url,
            ConnectionDirection::Inbound,
        ))
        .all(db)
        .await?
        .into_iter()
        .map(|r| (r.peer_id.to_string(), r.token_hash))
        .collect())
}

pub(super) struct TeamWorkerChallenge {
    pub team: String,
    pub token_hash: String,
    pub granted_projects: Vec<String>,
}

async fn lookup_team_worker_challenge(
    db: &WorkerDb,
    worker_id: &str,
) -> Result<Option<TeamWorkerChallenge>, DbErr> {
    let Some(worker) = load_active_team_worker_by_worker_id_and_connection_direction(
        db,
        worker_id,
        ConnectionDirection::Inbound,
    )
    .await?
    else {
        return Ok(None);
    };
    let granted_projects =
        gradient_db::teams::workers::projects_granted_with_workers(db, worker.team)
            .await?
            .into_iter()
            .map(|project| project.to_string())
            .collect();

    Ok(Some(TeamWorkerChallenge {
        team: worker.team.to_string(),
        token_hash: worker.token_hash,
        granted_projects,
    }))
}

pub(super) async fn load_inbound_worker_challenge(
    db: &WorkerDb,
    worker_id: &str,
) -> Result<(Option<TeamWorkerChallenge>, Vec<(String, String)>), DbErr> {
    match lookup_team_worker_challenge(db, worker_id).await? {
        Some(team) => {
            let peers = vec![(team.team.clone(), team.token_hash.clone())];
            Ok((Some(team), peers))
        }
        None => Ok((None, lookup_registered_peers(db, worker_id).await?)),
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

pub(super) struct CheckedWorkerTokens {
    pub proven_peer_token_hashes: Vec<(String, String)>,
    pub failed_peers: Vec<FailedPeer>,
}

static WORKER_TOKEN_CHECK_SEMAPHORE: LazyLock<Arc<Semaphore>> = LazyLock::new(|| {
    Arc::new(Semaphore::new(
        std::thread::available_parallelism().map_or(2, |n| n.get().div_ceil(2)),
    ))
});

async fn validate_tokens_on_limited_blocking_pool(
    registered: &[(String, String)],
    tokens: &[(String, SecretString)],
) -> anyhow::Result<(Vec<String>, Vec<FailedPeer>)> {
    let permit = Arc::clone(&WORKER_TOKEN_CHECK_SEMAPHORE)
        .acquire_owned()
        .await?;
    let (registered, tokens) = (registered.to_vec(), tokens.to_vec());
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        validate_tokens(&registered, &tokens)
    })
    .await
    .context("token verification task failed")
}

pub(super) async fn check_worker_tokens(
    registered: &[(String, String)],
    tokens: &[(String, SecretString)],
) -> anyhow::Result<CheckedWorkerTokens> {
    let (token_authorized, failed_peers) =
        validate_tokens_on_limited_blocking_pool(registered, tokens).await?;
    let proven_peer_token_hashes = registered
        .iter()
        .filter(|(peer_id, _)| token_authorized.contains(peer_id))
        .cloned()
        .collect();
    Ok(CheckedWorkerTokens {
        proven_peer_token_hashes,
        failed_peers,
    })
}

/// A capability is enabled only when every active registration is enabling it. The handshake is
/// clamping the advertised set with it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct EnabledCapsAggregate {
    pub enable_network: bool,
    pub enable_secrets: bool,
    pub enable_exec: bool,
    pub enable_eval: bool,
    pub enable_build: bool,
}

#[cfg(test)]
impl EnabledCapsAggregate {
    pub fn all() -> Self {
        Self {
            enable_network: true,
            enable_secrets: true,
            enable_exec: true,
            enable_eval: true,
            enable_build: true,
        }
    }
}

pub(super) async fn aggregate_enabled_caps(
    db: &WorkerDb,
    worker_id: &str,
    direction: ConnectionDirection,
) -> Result<EnabledCapsAggregate, DbErr> {
    use worker_registration::{Column, Entity};

    let rows = Entity::find()
        .filter(Column::WorkerId.eq(worker_id))
        .filter(Column::Active.eq(true))
        .filter(dial_url_sql_condition_for_connection_direction(
            Column::Url,
            direction,
        ))
        .all(db)
        .await?;

    if rows.is_empty() {
        let Some(worker) =
            load_active_team_worker_by_worker_id_and_connection_direction(db, worker_id, direction)
                .await?
        else {
            return Ok(EnabledCapsAggregate::default());
        };
        return Ok(EnabledCapsAggregate {
            enable_network: worker.enable_network,
            enable_secrets: worker.enable_secrets,
            enable_exec: worker.enable_exec,
            enable_eval: worker.enable_eval,
            enable_build: worker.enable_build,
        });
    }

    Ok(EnabledCapsAggregate {
        enable_network: rows.iter().all(|r| r.enable_network),
        enable_secrets: rows.iter().all(|r| r.enable_secrets),
        enable_exec: rows.iter().all(|r| r.enable_exec),
        enable_eval: rows.iter().all(|r| r.enable_eval),
        enable_build: rows.iter().all(|r| r.enable_build),
    })
}

pub(super) fn negotiate_capabilities(
    config: &RuntimeConfig,
    client: GradientCapabilities,
    enabled: EnabledCapsAggregate,
) -> GradientCapabilities {
    GradientCapabilities {
        core: true,
        cache: true,
        federate: client.federate && config.proto.federate,
        eval: client.eval && enabled.enable_eval,
        build: client.build && enabled.enable_build,
        network: client.network && enabled.enable_network,
        secrets: client.secrets && enabled.enable_secrets,
        exec: client.exec && enabled.enable_exec,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_core::ServerState;
    use gradient_fixtures::mock::mock_db;
    use gradient_test_support::state::TestState;
    use std::sync::Arc;

    fn all_caps(val: bool) -> GradientCapabilities {
        GradientCapabilities {
            core: val,
            cache: val,
            federate: val,
            eval: val,
            build: val,
            network: val,
            secrets: val,
            exec: val,
        }
    }

    fn make_state(federate_proto: bool) -> Arc<ServerState> {
        TestState::new()
            .config(move |config| config.proto.federate = federate_proto)
            .build()
    }

    #[test]
    fn negotiate_capabilities_core_always_true() {
        let state = make_state(false);
        let result =
            negotiate_capabilities(&state.config, all_caps(false), EnabledCapsAggregate::all());
        assert!(result.core);
    }

    #[test]
    fn negotiate_capabilities_cache_always_true() {
        let state = make_state(false);
        assert!(
            negotiate_capabilities(&state.config, all_caps(false), EnabledCapsAggregate::all())
                .cache
        );
        assert!(
            negotiate_capabilities(&state.config, all_caps(true), EnabledCapsAggregate::all())
                .cache
        );
    }

    #[test]
    fn negotiate_capabilities_federate_requires_both() {
        assert!(
            !negotiate_capabilities(
                &make_state(false).config,
                all_caps(true),
                EnabledCapsAggregate::all()
            )
            .federate
        );
        assert!(
            !negotiate_capabilities(
                &make_state(true).config,
                all_caps(false),
                EnabledCapsAggregate::all()
            )
            .federate
        );
        assert!(
            negotiate_capabilities(
                &make_state(true).config,
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
            eval: true,
            build: true,
            network: true,
            secrets: true,
            exec: true,
        };
        let result = negotiate_capabilities(&state.config, client, EnabledCapsAggregate::all());
        assert!(result.eval && result.build);
        assert!(result.network && result.secrets && result.exec);
    }

    #[test]
    fn negotiate_capabilities_clamped_by_enabled_aggregate() {
        let state = make_state(false);
        let client = all_caps(true);
        let enabled = EnabledCapsAggregate {
            enable_eval: true,
            enable_build: false,
            ..EnabledCapsAggregate::all()
        };
        let result = negotiate_capabilities(&state.config, client, enabled);
        assert!(result.eval);
        assert!(!result.build);
    }

    #[test]
    fn negotiation_keeps_network_secrets_and_exec_only_when_enabled() {
        let state = make_state(false);
        let enabled = EnabledCapsAggregate {
            enable_secrets: false,
            ..EnabledCapsAggregate::all()
        };
        let result = negotiate_capabilities(&state.config, all_caps(true), enabled);
        assert!(result.network && !result.secrets && result.exec);
    }

    use gradient_entity::project::Model as ProjectModel;
    use gradient_entity::project_cache::{CacheSubscriptionMode, Model as ProjectCacheModel};

    fn project_row(id: ProjectId) -> ProjectModel {
        ProjectModel {
            owner_team: None,
            id,
            name: format!("o-{}", id),
            display_name: "test".into(),
            description: String::new(),
            public_key: String::new(),
            private_key: String::new(),
            public: false,
            hide_build_requests: false,
            allow_exec_in_build_requests: false,
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
            ..Default::default()
        }
    }

    fn state_with_db(db: sea_orm::DatabaseConnection) -> Arc<ServerState> {
        TestState::new().worker_db(db).build()
    }

    #[tokio::test]
    async fn filter_project_peers_passes_through_project_with_cache() {
        let project = ProjectId::now_v7();
        let cache = gradient_types::ids::CacheId::now_v7();
        let db = mock_db()
            .append_query_results([vec![project_row(project)]])
            .append_query_results([vec![project_cache_row(project, cache)]])
            .into_connection();
        let state = state_with_db(db);
        let (authorized, demoted) =
            filter_project_peers_without_cache(&state.worker_db, vec![project.to_string()])
                .await
                .expect("peer filter");
        assert_eq!(authorized, vec![project.to_string()]);
        assert!(demoted.is_empty());
    }

    #[tokio::test]
    async fn filter_project_peers_demotes_project_without_cache() {
        let project = ProjectId::now_v7();
        let db = mock_db()
            .append_query_results([vec![project_row(project)]])
            .append_query_results([Vec::<ProjectCacheModel>::new()])
            .into_connection();
        let state = state_with_db(db);
        let (authorized, demoted) =
            filter_project_peers_without_cache(&state.worker_db, vec![project.to_string()])
                .await
                .expect("peer filter");
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
        let db = mock_db()
            .append_query_results([Vec::<ProjectModel>::new()])
            .into_connection();
        let state = state_with_db(db);
        let (authorized, demoted) =
            filter_project_peers_without_cache(&state.worker_db, vec![cache_peer.to_string()])
                .await
                .expect("peer filter");
        assert_eq!(authorized, vec![cache_peer.to_string()]);
        assert!(demoted.is_empty());
    }

    #[tokio::test]
    async fn a_database_error_rejects_instead_of_granting() {
        let failing = || {
            mock_db()
                .append_query_errors([DbErr::Custom("connection lost".into())])
                .into_connection()
        };

        let filtered = filter_project_peers_without_cache(
            &state_with_db(failing()).worker_db,
            vec![ProjectId::now_v7().to_string()],
        )
        .await;
        let caps = aggregate_enabled_caps(
            &state_with_db(failing()).worker_db,
            "w1",
            ConnectionDirection::Inbound,
        )
        .await;

        assert!(filtered.is_err());
        assert!(caps.is_err());
    }

    #[tokio::test]
    async fn a_worker_without_registrations_gets_no_capabilities() {
        let db = mock_db()
            .append_query_results([Vec::<gradient_entity::worker_registration::Model>::new()])
            .append_query_results([Vec::<gradient_entity::team_worker::Model>::new()])
            .into_connection();

        let caps = aggregate_enabled_caps(
            &state_with_db(db).worker_db,
            "w1",
            ConnectionDirection::Inbound,
        )
        .await
        .expect("caps");

        assert_eq!(caps, EnabledCapsAggregate::default());
    }

    #[tokio::test]
    async fn test_aggregate_enabled_caps_reads_only_session_direction_rows() {
        use gradient_fixtures::rows;

        let db = gradient_fixtures::database::TestDatabase::create().await;
        let conn = db.connection();
        let registration = worker_registration::Model {
            peer_id: rows::project().id,
            url: Some("wss://servers.gradient.ci/proto".into()),
            created_by: None,
            ..rows::worker_registration(true, true)
        };
        gradient_db::rows::insert::<gradient_types::EUser, _>(&conn, rows::user())
            .await
            .expect("the owner is written");
        gradient_db::rows::insert::<gradient_types::EProject, _>(&conn, rows::project())
            .await
            .expect("the project is written");
        gradient_db::rows::insert::<worker_registration::Entity, _>(&conn, registration.clone())
            .await
            .expect("the registration is written");
        let state = state_with_db(db.connection());

        let dialed = aggregate_enabled_caps(
            &state.worker_db,
            &registration.worker_id,
            ConnectionDirection::Outbound,
        )
        .await
        .expect("caps");
        let inbound = aggregate_enabled_caps(
            &state.worker_db,
            &registration.worker_id,
            ConnectionDirection::Inbound,
        )
        .await
        .expect("caps");

        assert_eq!(dialed, EnabledCapsAggregate::all());
        assert_eq!(inbound, EnabledCapsAggregate::default());
        db.finish().await;
    }

    #[tokio::test]
    async fn filter_project_peers_mixed() {
        let project_with = ProjectId::now_v7();
        let project_without = ProjectId::now_v7();
        let cache = gradient_types::ids::CacheId::now_v7();
        let cache_peer = ProjectId::now_v7();
        let db = mock_db()
            .append_query_results([vec![
                project_row(project_with),
                project_row(project_without),
            ]])
            .append_query_results([vec![project_cache_row(project_with, cache)]])
            .into_connection();
        let state = state_with_db(db);
        let (authorized, demoted) = filter_project_peers_without_cache(
            &state.worker_db,
            vec![
                project_with.to_string(),
                project_without.to_string(),
                cache_peer.to_string(),
            ],
        )
        .await
        .expect("peer filter");
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
}
