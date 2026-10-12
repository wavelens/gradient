/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_db::{DbContext, WebDb};
use std::sync::Arc;

use axum::extract::State;
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_entity::{team_worker, worker_registration};
use gradient_types::ids::{TeamId, TeamWorkerId, UserId, WorkerRegistrationId};
use gradient_types::{BaseResponse, MProject, MTeam, MTeamWorker, MUser};
use sea_orm::DbErr;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::access::{Caller, ProjectAccess, TeamAccess, load_project, load_team};
use crate::authorization::MaybeApiKey;
use crate::endpoints::projects::workers::encrypt_token;
use crate::error::{WebError, WebResult, unique_violation};
use crate::helpers::ok_json;
use crate::refusal::{Refusal, RefusalStatus};
use crate::resource::Resource;

#[derive(Debug, thiserror::Error)]
enum GradientCiConnectionRefusal {
    #[error("Gradient.CI Servers are turned off on this instance")]
    GradientCiDisabled,
    #[error("{0}")]
    InvalidConnectionToken(String),
    #[error("team is required for a team connection")]
    MissingTeam,
    #[error("a project connection names its project")]
    MissingProject,
    #[error("the {0} is already connected to Gradient.CI Servers")]
    AlreadyConnected(Resource),
    #[error("the worker of this connection token is already registered on this instance")]
    WorkerAlreadyRegistered,
}

impl Refusal for GradientCiConnectionRefusal {
    fn status(&self) -> RefusalStatus {
        match self {
            Self::GradientCiDisabled => RefusalStatus::Forbidden,
            Self::InvalidConnectionToken(_) | Self::MissingTeam | Self::MissingProject => {
                RefusalStatus::BadRequest
            }
            Self::AlreadyConnected(_) | Self::WorkerAlreadyRegistered => RefusalStatus::Conflict,
        }
    }
}

const DISPLAY_NAME: &str = "Gradient.CI Servers";
const ONE_CONNECTION_PER_TEAM: &str = "idx_team_worker_one_gradient_ci";
const PREFIX: &str = "gci1_";

#[derive(Deserialize, Clone, Copy, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionScope {
    Team,
    Project,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct ConnectionRequest {
    pub scope: ConnectionScope,
    pub team: Option<String>,
    pub project: Option<String>,
    pub token: String,
}

#[derive(Serialize, utoipa::ToSchema)]
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

#[utoipa::path(
    post,
    path = "/gradient-ci/connections",
    operation_id = "connectGradientCi",
    tag = "workers",
    summary = "Connect Gradient.CI Servers",
    description = "Stores a connection token pasted from servers.gradient.ci. `scope = project` creates a\nworker registration for the project (same permission as registering a worker);\n`scope = team` creates a worker of the team (Admin in the team). The server\ndials `wss://<gradientCi.url host>/proto` within 15 s, a team connection only once a\nproject grants the team its workers. Turned off by `gradientCi.enable = false`.",
    request_body = ConnectionRequest,
    responses((status = OK, description = "Connection stored", body = ConnectionResponse), WebError)
)]
pub async fn post_connection(
    State(state): State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Json(body): Json<ConnectionRequest>,
) -> WebResult<Json<BaseResponse<ConnectionResponse>>> {
    if !state.config.gradient_ci.enable {
        return Err(GradientCiConnectionRefusal::GradientCiDisabled.into());
    }

    let target = authorize_target(&state.db(), &user, &api_key, &body).await?;
    let token = ConnectionToken::parse(&body.token)
        .map_err(|e| GradientCiConnectionRefusal::InvalidConnectionToken(e.to_string()))?;
    let url = gradient_types::consts::GRADIENT_CI_PROTO_URL.to_owned();
    ensure_unclaimed(&state.web_db, &target, &token.worker_id).await?;
    let stored = StoredToken::new(&state.config.secrets.crypt_file, &token.secret)?;
    match target {
        Target::Team(team) => {
            insert_team_worker(&state.web_db, &user, &team, &token, url, stored).await?
        }
        Target::Project(project) => {
            insert_registration(&state.web_db, &user, &project, &token, url, stored).await?
        }
    }

    Ok(ok_json(ConnectionResponse {
        worker_id: token.worker_id,
    }))
}

async fn authorize_target(
    db: &DbContext,
    user: &MUser,
    api_key: &MaybeApiKey,
    body: &ConnectionRequest,
) -> WebResult<Target> {
    match body.scope {
        ConnectionScope::Team => {
            let name = body
                .team
                .clone()
                .ok_or(GradientCiConnectionRefusal::MissingTeam)?;
            let (team, _) = load_team(
                &db.web_db,
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
                .ok_or(GradientCiConnectionRefusal::MissingProject)?;
            let project = load_project(
                db,
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

async fn ensure_unclaimed(db: &WebDb, target: &Target, worker_id: &str) -> WebResult<()> {
    let (project, team) = match target {
        Target::Team(team) => (None, Some(team.id)),
        Target::Project(project) => (Some(project.id), None),
    };
    let registration = gradient_db::worker_registrations::by_worker_id_or_gradient_ci_project(
        db, worker_id, project,
    )
    .await?;
    let team_worker =
        gradient_db::teams::workers::by_worker_id_or_gradient_ci_team(db, worker_id, team).await?;

    match claim_conflict(
        target,
        registration.as_ref(),
        team_worker.as_ref(),
        worker_id,
    ) {
        Some(refusal) => Err(refusal.into()),
        None => Ok(()),
    }
}

fn claim_conflict(
    target: &Target,
    registration: Option<&worker_registration::Model>,
    team_worker: Option<&team_worker::Model>,
    worker_id: &str,
) -> Option<GradientCiConnectionRefusal> {
    let own_project = match target {
        Target::Project(project) => registration.is_some_and(|r| r.peer_id == project.id),
        Target::Team(_) => false,
    };
    if own_project {
        return Some(GradientCiConnectionRefusal::AlreadyConnected(
            Resource::Project,
        ));
    }

    if registration.is_some() || team_worker.is_some_and(|w| w.worker_id == worker_id) {
        return Some(GradientCiConnectionRefusal::WorkerAlreadyRegistered);
    }

    team_worker.map(|_| GradientCiConnectionRefusal::AlreadyConnected(Resource::Team))
}

async fn insert_registration(
    db: &WebDb,
    user: &MUser,
    project: &MProject,
    token: &ConnectionToken,
    url: String,
    stored: StoredToken,
) -> WebResult<()> {
    gradient_db::worker_registrations::insert(
        db,
        registration_row(project, user, token, url, stored),
    )
    .await
    .map_err(|e| {
        WebError::from_unique_violation(
            e,
            GradientCiConnectionRefusal::AlreadyConnected(Resource::Project),
        )
    })?;
    if let Err(e) = gradient_ci::unpark_no_workers_for_project(db, project.id).await {
        tracing::warn!(
            error = %e,
            project_id = %project.id,
            "failed to unpark no-workers evaluations after a Gradient.CI connection",
        );
    }

    Ok(())
}

fn team_worker_conflict(err: DbErr) -> WebError {
    match unique_violation(&err) {
        Some(ONE_CONNECTION_PER_TEAM) => {
            GradientCiConnectionRefusal::AlreadyConnected(Resource::Team).into()
        }
        Some(_) => GradientCiConnectionRefusal::WorkerAlreadyRegistered.into(),
        None => err.into(),
    }
}

async fn insert_team_worker(
    db: &WebDb,
    user: &MUser,
    team: &MTeam,
    token: &ConnectionToken,
    url: String,
    stored: StoredToken,
) -> WebResult<()> {
    gradient_db::teams::workers::register(
        db,
        team_worker_row(team.id, token.worker_id.clone(), url, stored, user.id),
    )
    .await
    .map_err(team_worker_conflict)?;

    let projects = gradient_db::teams::workers::projects_granted_with_workers(db, team.id).await?;
    for project in projects {
        if let Err(e) = gradient_ci::unpark_no_workers_for_project(db, project).await {
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
    fn a_project_connection_evaluates_and_builds_as_gradient_ci_servers() {
        let token = ConnectionToken::parse(&format!("gci1_{ID}_s")).expect("valid");
        let row = registration_row(
            &gradient_fixtures::rows::project(),
            &gradient_fixtures::rows::user(),
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
