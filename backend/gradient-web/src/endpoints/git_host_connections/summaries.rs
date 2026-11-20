/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! API keys that are restricted to one project see only the tasks of that project. Such keys see only webhook deliveries for that project's repositories on a shared connection.

use super::{
    GitHostConnectionSummary, IncomingWebhookDeliveryResponse, RepositoryFit, name_owner,
    webhook_url,
};
use crate::authorization::ApiKeyContext;
use crate::endpoints::git_host_hooks::repo_identity;
use crate::endpoints::tasks::repository::TaskCommitStatusPostAttempt;
use crate::error::WebResult;
use crate::permissions::{Permission, mask_grants};
use gradient_db::git_host_connections::{self as connections, GitHostConnectionOwnerByDatabaseId};
use gradient_entity::incoming_webhook_delivery;
use gradient_types::git_host::GitHostConnectionOwner;
use gradient_types::*;
use std::collections::{HashMap, HashSet};

pub(crate) struct ConnectionReader<'a> {
    pub(crate) user: &'a MUser,
    pub(crate) pinned_project: Option<ProjectId>,
}

impl<'a> ConnectionReader<'a> {
    pub(crate) fn new(user: &'a MUser, api_key: Option<&ApiKeyContext>) -> Self {
        Self {
            user,
            pinned_project: api_key.and_then(|key| key.project),
        }
    }

    fn sees_every_delivery_of(&self, connection: &MGitHostConnection) -> bool {
        self.pinned_project.is_none_or(|pinned| {
            GitHostConnectionOwnerByDatabaseId::of(connection)
                == GitHostConnectionOwnerByDatabaseId::Project(pinned)
        })
    }
}

pub(crate) async fn project_names_visible_to_reader(
    db: &impl sea_orm::ConnectionTrait,
    reader: &ConnectionReader<'_>,
    project_ids: impl IntoIterator<Item = ProjectId>,
) -> WebResult<HashMap<ProjectId, String>> {
    let candidates: Vec<ProjectId> = project_ids
        .into_iter()
        .collect::<HashSet<_>>()
        .into_iter()
        .filter(|project| {
            reader
                .pinned_project
                .is_none_or(|pinned| pinned == *project)
        })
        .collect();
    if candidates.is_empty() {
        return Ok(HashMap::new());
    }
    let user = reader.user;
    let grants = if user.superuser {
        HashMap::new()
    } else {
        gradient_db::access::project_grants(db, user.id, candidates.clone()).await?
    };
    Ok(gradient_db::projects::with_ids(db, &candidates)
        .await?
        .into_iter()
        .filter(|project| {
            user.superuser
                || project.public
                || grants
                    .get(&project.id)
                    .is_some_and(|grant| mask_grants(grant.permissions, Permission::ViewProject))
        })
        .map(|project| (project.id, project.name))
        .collect())
}

pub(crate) struct DeliveryRepositoryLimits(HashMap<GitHostConnectionId, HashSet<String>>);

impl DeliveryRepositoryLimits {
    pub(crate) async fn of(
        db: &impl sea_orm::ConnectionTrait,
        reader: &ConnectionReader<'_>,
        connections: &[&MGitHostConnection],
    ) -> WebResult<Self> {
        let mut limits: HashMap<GitHostConnectionId, HashSet<String>> = connections
            .iter()
            .filter(|connection| !reader.sees_every_delivery_of(connection))
            .map(|connection| (connection.id, HashSet::new()))
            .collect();
        if let Some(pinned) = reader.pinned_project
            && !limits.is_empty()
        {
            let limited: Vec<GitHostConnectionId> = limits.keys().copied().collect();
            for (connection, repository) in
                connections::repositories_of_project_tasks_by_connection(db, pinned, &limited)
                    .await?
            {
                if let (Some(repositories), Some(identity)) =
                    (limits.get_mut(&connection), repo_identity(&repository))
                {
                    repositories.insert(identity);
                }
            }
        }
        Ok(Self(limits))
    }

    pub(crate) fn allow(&self, delivery: &incoming_webhook_delivery::Model) -> bool {
        match self.0.get(&delivery.git_host_connection) {
            None => true,
            Some(repositories) => delivery
                .repository
                .as_deref()
                .and_then(repo_identity)
                .is_some_and(|identity| repositories.contains(&identity)),
        }
    }
}

pub(crate) struct SummaryOptions<'a> {
    pub(crate) serve_url: &'a str,
    pub(crate) repository: Option<&'a str>,
}

pub(crate) struct ConnectionToSummarize {
    pub(crate) connection: MGitHostConnection,
    pub(crate) can_manage: bool,
}

pub(crate) async fn summaries(
    db: &impl sea_orm::ConnectionTrait,
    reader: &ConnectionReader<'_>,
    options: &SummaryOptions<'_>,
    rows: Vec<ConnectionToSummarize>,
) -> WebResult<Vec<GitHostConnectionSummary>> {
    let ids: Vec<GitHostConnectionId> = rows.iter().map(|row| row.connection.id).collect();

    let counts = connections::task_counts_by_connection_and_project(db, &ids).await?;
    let visible_projects =
        project_names_visible_to_reader(db, reader, counts.iter().map(|(_, project, _)| *project))
            .await?;
    let mut task_counts: HashMap<GitHostConnectionId, u64> = HashMap::new();
    for (connection, project, count) in counts {
        if visible_projects.contains_key(&project) {
            *task_counts.entry(connection).or_default() += u64::try_from(count).unwrap_or(0);
        }
    }

    let visible_project_ids: Vec<ProjectId> = visible_projects.keys().copied().collect();
    let mut status_posts: HashMap<GitHostConnectionId, _> =
        connections::newest_commit_status_post_attempt_per_connection(
            db,
            &ids,
            &visible_project_ids,
        )
        .await?
        .into_iter()
        .filter_map(|post| Some((post.git_host_connection?, post)))
        .collect();

    let connection_refs: Vec<&MGitHostConnection> =
        rows.iter().map(|row| &row.connection).collect();
    let limits = DeliveryRepositoryLimits::of(db, reader, &connection_refs).await?;
    let mut last_deliveries: HashMap<GitHostConnectionId, incoming_webhook_delivery::Model> =
        HashMap::new();
    for delivery in gradient_db::incoming_deliveries::newest_per_connection_and_repository(db, &ids)
        .await?
        .into_iter()
        .filter(|delivery| limits.allow(delivery))
    {
        let newer = last_deliveries
            .get(&delivery.git_host_connection)
            .is_none_or(|known| known.created_at < delivery.created_at);
        if newer {
            last_deliveries.insert(delivery.git_host_connection, delivery);
        }
    }

    let mut owner_names: Vec<(GitHostConnectionOwnerByDatabaseId, GitHostConnectionOwner)> =
        Vec::new();
    let mut out = Vec::with_capacity(rows.len());
    for ConnectionToSummarize {
        connection,
        can_manage,
    } in rows
    {
        let owner_id = GitHostConnectionOwnerByDatabaseId::of(&connection);
        let owner = match owner_names.iter().find(|(id, _)| *id == owner_id) {
            Some((_, named)) => named.clone(),
            None => {
                let named = name_owner(db, owner_id).await?;
                owner_names.push((owner_id, named.clone()));
                named
            }
        };
        out.push(GitHostConnectionSummary {
            id: connection.id,
            owner,
            name: connection.name.clone(),
            display_name: connection.display_name.clone(),
            git_host_type: connection.git_host_type.as_path_segment().to_owned(),
            base_url: connection.base_url.clone(),
            receives_events: connection.receives_events,
            reports_status: connection.reports_status,
            can_manage,
            managed: connection.managed,
            account_login: connection.account_login.clone(),
            github_installation_id: connection.github_installation_id,
            webhook_url: webhook_url(options.serve_url, &connection),
            access_token_expires_at: connection.access_token_expires_at,
            access_token_refreshes_automatically: connection.encrypted_refresh_token.is_some(),
            last_delivery: last_deliveries
                .remove(&connection.id)
                .map(IncomingWebhookDeliveryResponse::from),
            refused_signature_count: connection.refused_signature_count,
            last_refused_signature_at: connection.last_refused_signature_at,
            last_status_post: status_posts
                .remove(&connection.id)
                .map(|post| TaskCommitStatusPostAttempt::of(post, can_manage)),
            task_count: task_counts.get(&connection.id).copied().unwrap_or(0),
            repository_fit: options
                .repository
                .map(|repository| RepositoryFit::of(repository, &connection)),
        });
    }
    Ok(out)
}

pub(crate) async fn summaries_of_owner(
    db: &impl sea_orm::ConnectionTrait,
    reader: &ConnectionReader<'_>,
    owner: GitHostConnectionOwnerByDatabaseId,
    options: &SummaryOptions<'_>,
    can_manage: bool,
) -> WebResult<Vec<GitHostConnectionSummary>> {
    let rows = connections::of_owner(db, owner)
        .await?
        .into_iter()
        .map(|connection| ConnectionToSummarize {
            connection,
            can_manage,
        })
        .collect();
    summaries(db, reader, options, rows).await
}
