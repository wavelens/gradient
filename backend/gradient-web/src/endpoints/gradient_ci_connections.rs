/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;

use axum::extract::State;
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_entity::{team_worker, worker_registration};
use gradient_types::ids::{TeamId, TeamWorkerId, UserId, WorkerRegistrationId};
use gradient_types::{
    BaseResponse, ETeamWorker, EWorkerRegistration, MProject, MTeam, MTeamWorker, MUser,
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, IntoActiveModel, QueryFilter,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::access::{Caller, ProjectAccess, TeamAccess, load_project, load_team};
use crate::authorization::MaybeApiKey;
use crate::endpoints::projects::workers::encrypt_token;
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;

const DISPLAY_NAME: &str = "Gradient.CI Servers";
const PREFIX: &str = "gci1_";

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionScope {
    Team,
    Project,
}

#[derive(Deserialize)]
pub struct ConnectionRequest {
    pub scope: ConnectionScope,
    pub team: Option<String>,
    pub project: Option<String>,
    pub token: String,
}

#[derive(Serialize)]
pub struct ConnectionResponse {
    pub worker_id: String,
}

pub(crate) struct ConnectionToken {
    pub worker_id: String,
    pub secret: String,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum TokenFormatError {
    #[error("a connection token starts with gci1_")]
    Prefix,
    #[error("a connection token names a worker UUID after gci1_")]
    WorkerId,
    #[error("the connection token is missing its secret part")]
    Secret,
}

struct StoredToken {
    hash: String,
    encrypted: String,
}

enum Target {
    Team(MTeam),
    Project(MProject),
}

impl ConnectionToken {
    pub(crate) fn parse(raw: &str) -> Result<Self, TokenFormatError> {
        let rest = raw
            .trim()
            .strip_prefix(PREFIX)
            .ok_or(TokenFormatError::Prefix)?;
        let (worker_id, secret) = rest.split_once('_').ok_or(TokenFormatError::Secret)?;
        let worker_id = Uuid::parse_str(worker_id).map_err(|_| TokenFormatError::WorkerId)?;
        if secret.is_empty() || secret.chars().any(char::is_whitespace) {
            return Err(TokenFormatError::Secret);
        }

        Ok(Self {
            worker_id: worker_id.to_string(),
            secret: secret.to_owned(),
        })
    }
}

impl StoredToken {
    fn new(crypt_file: &str, secret: &str) -> WebResult<Self> {
        Ok(Self {
            hash: password_auth::generate_hash(secret),
            encrypted: encrypt_token(crypt_file, secret)?,
        })
    }
}

pub async fn post_connection(
    State(state): State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Json(body): Json<ConnectionRequest>,
) -> WebResult<Json<BaseResponse<ConnectionResponse>>> {
    if !state.config.gradient_ci.enable {
        return Err(WebError::forbidden(
            "Gradient.CI Servers are turned off on this instance",
        ));
    }

    let target = authorize_target(&state, &user, &api_key, &body).await?;
    let token =
        ConnectionToken::parse(&body.token).map_err(|e| WebError::bad_request(e.to_string()))?;
    let url = proto_url(&state.config.gradient_ci.url)
        .ok_or_else(|| WebError::internal("gradientCi.url is not a valid URL"))?;
    ensure_unclaimed(&state, &target, &token.worker_id).await?;
    let stored = StoredToken::new(&state.config.secrets.crypt_file, &token.secret)?;
    match target {
        Target::Team(team) => insert_team_worker(&state, &user, &team, &token, url, stored).await?,
        Target::Project(project) => {
            insert_registration(&state, &user, &project, &token, url, stored).await?
        }
    }

    Ok(ok_json(ConnectionResponse {
        worker_id: token.worker_id,
    }))
}

async fn authorize_target(
    state: &Arc<ServerState>,
    user: &MUser,
    api_key: &MaybeApiKey,
    body: &ConnectionRequest,
) -> WebResult<Target> {
    match body.scope {
        ConnectionScope::Team => {
            let name = body
                .team
                .clone()
                .ok_or_else(|| WebError::bad_request("team is required for a team connection"))?;
            let (team, _) = load_team(
                state,
                user,
                api_key.as_ref(),
                name,
                TeamAccess::Admin {
                    reject_managed: false,
                },
            )
            .await?;
            Ok(Target::Team(team))
        }
        ConnectionScope::Project => {
            let name = body
                .project
                .clone()
                .ok_or_else(|| WebError::bad_request("a project connection names its project"))?;
            let project = load_project(
                state,
                Caller::User(user),
                api_key.as_ref(),
                name,
                ProjectAccess::Member {
                    reject_managed: false,
                },
            )
            .await?;
            Ok(Target::Project(project))
        }
    }
}

fn proto_url(service_url: &str) -> Option<String> {
    let parsed = url::Url::parse(service_url).ok()?;
    let scheme = match parsed.scheme() {
        "https" => "wss",
        "http" => "ws",
        _ => return None,
    };
    let host = parsed.host_str()?;
    Some(match parsed.port() {
        Some(port) => format!("{scheme}://{host}:{port}/proto"),
        None => format!("{scheme}://{host}/proto"),
    })
}

async fn ensure_unclaimed(state: &ServerState, target: &Target, worker_id: &str) -> WebResult<()> {
    let same_worker = Condition::any().add(worker_registration::Column::WorkerId.eq(worker_id));
    let registrations = match target {
        Target::Team(_) => same_worker,
        Target::Project(project) => same_worker.add(
            Condition::all()
                .add(worker_registration::Column::PeerId.eq(project.id))
                .add(worker_registration::Column::GradientCi.eq(true)),
        ),
    };

    let registration = EWorkerRegistration::find()
        .filter(registrations)
        .one(&state.web_db)
        .await?;

    let mut team_workers = Condition::any().add(team_worker::Column::WorkerId.eq(worker_id));
    if let Target::Team(team) = target {
        team_workers = team_workers.add(
            Condition::all()
                .add(team_worker::Column::Team.eq(team.id))
                .add(team_worker::Column::GradientCi.eq(true)),
        );
    }

    let team_worker = ETeamWorker::find()
        .filter(team_workers)
        .one(&state.web_db)
        .await?;

    match claim_conflict(
        target,
        registration.as_ref(),
        team_worker.as_ref(),
        worker_id,
    ) {
        Some(message) => Err(WebError::conflict(message)),
        None => Ok(()),
    }
}

fn claim_conflict(
    target: &Target,
    registration: Option<&worker_registration::Model>,
    team_worker: Option<&team_worker::Model>,
    worker_id: &str,
) -> Option<&'static str> {
    let own_project = match target {
        Target::Project(project) => registration.is_some_and(|r| r.peer_id == project.id),
        Target::Team(_) => false,
    };
    if own_project {
        return Some("the project is already connected to Gradient.CI Servers");
    }

    if registration.is_some() || team_worker.is_some_and(|w| w.worker_id == worker_id) {
        return Some("the worker of this connection token is already registered on this instance");
    }

    team_worker.map(|_| "the team is already connected to Gradient.CI Servers")
}

async fn insert_registration(
    state: &ServerState,
    user: &MUser,
    project: &MProject,
    token: &ConnectionToken,
    url: String,
    stored: StoredToken,
) -> WebResult<()> {
    registration_row(project, user, token, url, stored)
        .into_active_model()
        .insert(&state.web_db)
        .await
        .map_err(|e| WebError::from_db_err(e, "Gradient.CI connection"))?;
    if let Err(e) = gradient_ci::unpark_no_workers_for_project(&state.web_db, project.id).await {
        tracing::warn!(
            error = %e,
            project_id = %project.id,
            "failed to unpark no-workers evaluations after a Gradient.CI connection",
        );
    }

    Ok(())
}

async fn insert_team_worker(
    state: &ServerState,
    user: &MUser,
    team: &MTeam,
    token: &ConnectionToken,
    url: String,
    stored: StoredToken,
) -> WebResult<()> {
    team_worker_row(team.id, token.worker_id.clone(), url, stored, user.id)
        .into_active_model()
        .insert(&state.web_db)
        .await
        .map_err(|e| WebError::from_db_err(e, "Gradient.CI connection"))?;

    let projects =
        gradient_db::teams::workers::projects_granted_with_workers(&state.web_db, team.id).await?;
    for project in projects {
        if let Err(e) = gradient_ci::unpark_no_workers_for_project(&state.web_db, project).await {
            tracing::warn!(
                error = %e,
                project_id = %project,
                "failed to unpark no-workers evaluations after a Gradient.CI connection",
            );
        }
    }

    Ok(())
}

fn registration_row(
    project: &MProject,
    user: &MUser,
    token: &ConnectionToken,
    url: String,
    stored: StoredToken,
) -> worker_registration::Model {
    worker_registration::Model {
        id: WorkerRegistrationId::now_v7(),
        peer_id: project.id,
        worker_id: token.worker_id.clone(),
        token_hash: stored.hash,
        token_encrypted: Some(stored.encrypted),
        managed: false,
        url: Some(url),
        active: true,
        enable_fetch: false,
        enable_eval: true,
        enable_build: true,
        display_name: DISPLAY_NAME.into(),
        gradient_ci: true,
        created_by: Some(user.id),
        created_at: gradient_types::now(),
    }
}

fn team_worker_row(
    team: TeamId,
    worker_id: String,
    url: String,
    token: StoredToken,
    user: UserId,
) -> MTeamWorker {
    MTeamWorker {
        id: TeamWorkerId::now_v7(),
        team,
        worker_id,
        token_hash: token.hash,
        token_encrypted: Some(token.encrypted),
        url: Some(url),
        display_name: DISPLAY_NAME.into(),
        gradient_ci: true,
        enable_fetch: false,
        enable_eval: true,
        enable_build: true,
        active: true,
        managed: false,
        created_by: Some(user),
        created_at: gradient_types::now(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "0199a0b1-c2d3-7e4f-8a6b-9c0d1e2f3a4b";

    #[test]
    fn a_pasted_token_survives_surrounding_whitespace() {
        let token = ConnectionToken::parse(&format!("  gci1_{ID}_s3cr3t_part\n")).expect("valid");
        assert_eq!(token.worker_id, ID);
        assert_eq!(token.secret, "s3cr3t_part");
    }

    #[test]
    fn an_uppercase_worker_id_is_normalized() {
        let token =
            ConnectionToken::parse(&format!("gci1_{}_s", ID.to_uppercase())).expect("valid");
        assert_eq!(token.worker_id, ID);
    }

    #[test]
    fn a_misplaced_paste_names_the_expected_prefix() {
        assert_eq!(
            ConnectionToken::parse("GRADabcdef").err(),
            Some(TokenFormatError::Prefix)
        );
        assert_eq!(
            ConnectionToken::parse("wss://servers.gradient.ci/proto").err(),
            Some(TokenFormatError::Prefix)
        );
        assert!(TokenFormatError::Prefix.to_string().contains("gci1_"));
    }

    #[test]
    fn a_broken_token_is_refused() {
        assert_eq!(
            ConnectionToken::parse("gci1_not-a-uuid_s").err(),
            Some(TokenFormatError::WorkerId)
        );
        assert_eq!(
            ConnectionToken::parse(&format!("gci1_{ID}")).err(),
            Some(TokenFormatError::Secret)
        );
        assert_eq!(
            ConnectionToken::parse(&format!("gci1_{ID}_")).err(),
            Some(TokenFormatError::Secret)
        );
        assert_eq!(
            ConnectionToken::parse(&format!("gci1_{ID}_se\ncret")).err(),
            Some(TokenFormatError::Secret)
        );
    }

    #[test]
    fn the_connection_dials_the_service_host_with_the_matching_websocket_scheme() {
        assert_eq!(
            proto_url("https://servers.gradient.ci").as_deref(),
            Some("wss://servers.gradient.ci/proto")
        );
        assert_eq!(
            proto_url("http://localhost:3200/").as_deref(),
            Some("ws://localhost:3200/proto")
        );
        assert_eq!(proto_url("not a url"), None);
    }

    #[test]
    fn a_project_connection_evaluates_and_builds_as_gradient_ci_servers() {
        let token = ConnectionToken::parse(&format!("gci1_{ID}_s")).expect("valid");
        let row = registration_row(
            &gradient_test_support::fixtures::project(),
            &gradient_test_support::fixtures::user(),
            &token,
            "wss://servers.gradient.ci/proto".into(),
            StoredToken {
                hash: "hash".into(),
                encrypted: "encrypted".into(),
            },
        );

        assert!(row.gradient_ci && row.active && row.enable_eval && row.enable_build);
        assert!(!row.enable_fetch && !row.managed);
        assert_eq!(row.token_encrypted.as_deref(), Some("encrypted"));
        assert_eq!(row.url.as_deref(), Some("wss://servers.gradient.ci/proto"));
    }
}
