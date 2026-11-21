/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::{
    GitHostConnectionRefusal, audit_log_owner_of_git_host_connection,
    link_fitting_tasks_with_repository_grant, load_of_owner,
};
use crate::audit::Audit;
use crate::error::{WebError, WebResult};
use crate::helpers::OptionExt;
use crate::resource::Resource;
use gradient_db::git_host_connection_repository_grants::{
    self as grants, NewGitHostConnectionRepositoryGrant,
};
use gradient_db::git_host_connections::GitHostConnectionOwnerByDatabaseId;
use gradient_types::events::audit::Action;
use gradient_types::*;
use sea_orm::{TransactionSession, TransactionTrait};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Deserialize, Debug, utoipa::ToSchema)]
pub struct CreateRepositoryGrantRequest {
    pub project: String,
    pub repository_owner: String,
    pub repository_name: Option<String>,
}

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct RepositoryGrantResponse {
    pub id: GitHostConnectionRepositoryGrantId,
    pub project: ProjectId,
    pub project_name: Option<String>,
    pub repository_owner: String,
    pub repository_name: Option<String>,
    #[schema(value_type = String)]
    pub source: RepositoryGrantSource,
    pub created_by: Option<UserId>,
    pub created_at: chrono::NaiveDateTime,
}

impl RepositoryGrantResponse {
    fn new(grant: MGitHostConnectionRepositoryGrant, project_name: Option<String>) -> Self {
        Self {
            id: grant.id,
            project: grant.project,
            project_name,
            repository_owner: grant.repository_owner,
            repository_name: grant.repository_name,
            source: grant.source,
            created_by: grant.created_by,
            created_at: grant.created_at,
        }
    }
}

const REPOSITORY_OWNER_OR_REPOSITORY_NAME_MAX_LENGTH_IN_BYTES: usize = 255;

fn repository_grant_field_too_long_error(
    field: &'static str,
    value: &str,
) -> Result<(), GitHostConnectionRefusal> {
    if value.len() <= REPOSITORY_OWNER_OR_REPOSITORY_NAME_MAX_LENGTH_IN_BYTES {
        return Ok(());
    }
    Err(
        GitHostConnectionRefusal::RepositoryGrantOwnerOrRepositoryNameExceedsMaximumLength {
            field,
            max_length: REPOSITORY_OWNER_OR_REPOSITORY_NAME_MAX_LENGTH_IN_BYTES,
        },
    )
}

fn validate_repository_owner_length_and_url_path_segment(
    owner: &str,
) -> Result<(), GitHostConnectionRefusal> {
    repository_grant_field_too_long_error("repository_owner", owner)?;
    if is_plain_name_segment(owner) {
        return Ok(());
    }
    Err(
        GitHostConnectionRefusal::RepositoryGrantOwnerNotSingleUrlPathSegment {
            field: "repository_owner",
            value: owner.to_owned(),
        },
    )
}

/// A name can span multiple segments (GitLab subgroups). The code in `parse_owner_repo` can take
/// everything after the owner as the name and remove a trailing `.git`.
fn validate_repository_name(name: &str) -> Result<(), GitHostConnectionRefusal> {
    repository_grant_field_too_long_error("repository_name", name)?;
    if !name.split('/').all(is_plain_name_segment) {
        return Err(
            GitHostConnectionRefusal::RepositoryGrantRepositoryNameNotValidRepositoryPath {
                value: name.to_owned(),
            },
        );
    }
    if name.ends_with(".git") {
        return Err(GitHostConnectionRefusal::RepositoryGrantNameEndsInGit {
            value: name.to_owned(),
        });
    }
    Ok(())
}

async fn write_audit(
    audit: &Audit,
    action: Action,
    connection: &MGitHostConnection,
    grant: &MGitHostConnectionRepositoryGrant,
    taken_over_from_source: Option<RepositoryGrantSource>,
    git_host_connection_linked_task_ids: Option<&[TaskId]>,
) {
    let mut details = serde_json::json!({
        "git_host_connection_id": connection.id.to_string(),
        "repository_grant_id": grant.id.to_string(),
        "project_id": grant.project.to_string(),
        "repository_owner": grant.repository_owner,
        "repository_name": grant.repository_name,
        "source": grant.source,
    });
    if let Some(former) = taken_over_from_source {
        details["taken_over_from_source"] = serde_json::json!(former);
    }
    if let Some(linked) = git_host_connection_linked_task_ids {
        details["git_host_connection_linked_task_ids"] =
            serde_json::json!(linked.iter().map(ToString::to_string).collect::<Vec<_>>());
    }
    audit
        .write(
            action,
            audit_log_owner_of_git_host_connection(connection),
            details,
        )
        .await;
}

pub(crate) async fn list(
    db: &impl sea_orm::ConnectionTrait,
    owner: GitHostConnectionOwnerByDatabaseId,
    connection: GitHostConnectionId,
) -> WebResult<Vec<RepositoryGrantResponse>> {
    load_of_owner(db, owner, connection).await?;
    let rows = grants::of_connection(db, connection).await?;
    let project_ids: Vec<ProjectId> = rows.iter().map(|grant| grant.project).collect();
    let visible_project_names: HashMap<ProjectId, String> =
        gradient_db::projects::with_ids(db, &project_ids)
            .await?
            .into_iter()
            .filter(|project| match owner {
                GitHostConnectionOwnerByDatabaseId::Team(team) => project.owner_team == Some(team),
                _ => true,
            })
            .map(|project| (project.id, project.name))
            .collect();
    Ok(rows
        .into_iter()
        .map(|grant| {
            let project_name = visible_project_names.get(&grant.project).cloned();
            RepositoryGrantResponse::new(grant, project_name)
        })
        .collect())
}

pub(crate) async fn create(
    db: &(impl sea_orm::ConnectionTrait + TransactionTrait),
    owner: GitHostConnectionOwnerByDatabaseId,
    connection: GitHostConnectionId,
    created_by: UserId,
    body: CreateRepositoryGrantRequest,
    audit: &Audit,
) -> WebResult<RepositoryGrantResponse> {
    let connection = load_of_owner(db, owner, connection).await?;
    validate_repository_owner_length_and_url_path_segment(&body.repository_owner)?;
    if let Some(name) = &body.repository_name {
        validate_repository_name(name)?;
    }
    let project = gradient_db::projects::by_name(db, &body.project).await?;
    let project = match owner {
        GitHostConnectionOwnerByDatabaseId::Team(team) => project
            .filter(|project| project.owner_team == Some(team))
            .ok_or_else(
                || GitHostConnectionRefusal::RepositoryGrantProjectNotInTeam {
                    project: body.project.clone(),
                },
            )?,
        _ => project.or_not_found(Resource::Project)?,
    };
    let txn = db.begin().await?;
    let existing = grants::of_connection_project_and_repository(
        &txn,
        connection.id,
        project.id,
        &body.repository_owner,
        body.repository_name.as_deref(),
    )
    .await?;
    let (grant, taken_over_from_source, linked) = match existing {
        Some(existing) if existing.source == RepositoryGrantSource::Admin => {
            return Err(WebError::already_exists(
                Resource::GitHostConnectionRepositoryGrant,
            ));
        }
        Some(existing) => {
            let former_source = existing.source;
            let grant = grants::take_over_as_admin_grant(&txn, existing, created_by).await?;
            (grant, Some(former_source), Vec::new())
        }
        None => {
            let grant = grants::insert(
                &txn,
                NewGitHostConnectionRepositoryGrant {
                    connection: connection.id,
                    project: project.id,
                    repository_owner: &body.repository_owner,
                    repository_name: body.repository_name.as_deref(),
                    source: RepositoryGrantSource::Admin,
                    created_by: Some(created_by),
                },
            )
            .await
            .map_err(|err| {
                WebError::from_db_err(err, Resource::GitHostConnectionRepositoryGrant)
            })?;
            let candidates = gradient_db::tasks::of_project(project.id).all(&txn).await?;
            let linked =
                link_fitting_tasks_with_repository_grant(&txn, &connection, candidates).await?;
            (grant, None, linked)
        }
    };
    txn.commit().await?;
    let linked_ids: Vec<TaskId> = linked.iter().map(|task| task.id).collect();
    write_audit(
        audit,
        Action::GitHostConnectionRepositoryGrantCreate,
        &connection,
        &grant,
        taken_over_from_source,
        Some(&linked_ids),
    )
    .await;
    Ok(RepositoryGrantResponse::new(grant, Some(project.name)))
}

pub(crate) async fn delete(
    db: &impl sea_orm::ConnectionTrait,
    owner: GitHostConnectionOwnerByDatabaseId,
    connection: GitHostConnectionId,
    grant: GitHostConnectionRepositoryGrantId,
    audit: &Audit,
) -> WebResult<()> {
    let connection = load_of_owner(db, owner, connection).await?;
    let grant = grants::find_one_of_connection_by_id(db, connection.id, grant)
        .await?
        .or_not_found(Resource::GitHostConnectionRepositoryGrant)?;
    grants::delete_of_connection_by_id(db, connection.id, grant.id).await?;
    write_audit(
        audit,
        Action::GitHostConnectionRepositoryGrantDelete,
        &connection,
        &grant,
        None,
        None,
    )
    .await;
    Ok(())
}
