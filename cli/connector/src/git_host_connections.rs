// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

use crate::api::{
    CreateGitHostConnectionRequest, GitHostConnectionOwner, GitHostConnectionResponse,
    GitHostConnectionSummary, IncomingWebhookDeliveryResponse, PatchGitHostConnectionRequest,
};
use crate::http::encode_url_component;
use crate::{Client, ConnectorError, http};
use reqwest::{Method, RequestBuilder};
use serde::de::DeserializeOwned;
use std::fmt;
use std::str::FromStr;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitHostConnectionSetOwner {
    Instance,
    Team(String),
    Project(String),
}

impl GitHostConnectionSetOwner {
    fn git_host_connections_path_prefix(&self) -> String {
        match self {
            Self::Instance => "admin/git-host-connections".to_owned(),
            Self::Team(team) => format!("teams/{}/git-host-connections", encode_url_component(team)),
            Self::Project(project) => format!("projects/{}/git-host-connections", encode_url_component(project)),
        }
    }
}

impl From<&GitHostConnectionOwner> for GitHostConnectionSetOwner {
    fn from(owner: &GitHostConnectionOwner) -> Self {
        match owner {
            GitHostConnectionOwner::Instance => Self::Instance,
            GitHostConnectionOwner::Team(team) => Self::Team(team.clone()),
            GitHostConnectionOwner::Project(project) => Self::Project(project.clone()),
        }
    }
}

impl fmt::Display for GitHostConnectionSetOwner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Instance => f.write_str("instance"),
            Self::Team(team) => write!(f, "team/{team}"),
            Self::Project(project) => write!(f, "project/{project}"),
        }
    }
}

impl FromStr for GitHostConnectionSetOwner {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.split_once('/') {
            None if value == "instance" => Ok(Self::Instance),
            Some(("team", team)) if !team.is_empty() => Ok(Self::Team(team.to_owned())),
            Some(("project", project)) if !project.is_empty() => {
                Ok(Self::Project(project.to_owned()))
            }
            _ => Err("Use `instance`, `team/<name>` or `project/<name>` for the owner.".to_owned()),
        }
    }
}

pub struct GitHostConnections<'a>(pub(crate) &'a Client);

impl GitHostConnections<'_> {
    fn request(
        &self,
        method: Method,
        owner: &GitHostConnectionSetOwner,
        suffix: &str,
    ) -> Result<RequestBuilder, ConnectorError> {
        http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            method,
            &format!("{}{suffix}", owner.git_host_connections_path_prefix()),
            true,
        )
    }

    async fn send<T: DeserializeOwned>(req: RequestBuilder) -> Result<T, ConnectorError> {
        http::decode(req.send().await?).await
    }

    pub async fn list(
        &self,
        owner: &GitHostConnectionSetOwner,
    ) -> Result<Vec<GitHostConnectionSummary>, ConnectorError> {
        Self::send(self.request(Method::GET, owner, "")?).await
    }

    pub async fn create(
        &self,
        owner: &GitHostConnectionSetOwner,
        body: &CreateGitHostConnectionRequest,
    ) -> Result<GitHostConnectionResponse, ConnectorError> {
        Self::send(self.request(Method::PUT, owner, "")?.json(body)).await
    }

    pub async fn get(
        &self,
        owner: &GitHostConnectionSetOwner,
        id: &str,
    ) -> Result<GitHostConnectionResponse, ConnectorError> {
        Self::send(self.request(Method::GET, owner, \&format!("/{}", encode_url_component(id)))?).await
    }

    pub async fn update(
        &self,
        owner: &GitHostConnectionSetOwner,
        id: &str,
        body: &PatchGitHostConnectionRequest,
    ) -> Result<GitHostConnectionResponse, ConnectorError> {
        Self::send(
            self.request(Method::PATCH, owner, \&format!("/{}", encode_url_component(id)))?
                .json(body),
        )
        .await
    }

    pub async fn delete(
        &self,
        owner: &GitHostConnectionSetOwner,
        id: &str,
    ) -> Result<bool, ConnectorError> {
        Self::send(self.request(Method::DELETE, owner, \&format!("/{}", encode_url_component(id)))?).await
    }

    pub async fn list_latest_webhook_deliveries_for_git_host_connection(
        &self,
        owner: &GitHostConnectionSetOwner,
        id: &str,
    ) -> Result<Vec<IncomingWebhookDeliveryResponse>, ConnectorError> {
        Self::send(self.request(Method::GET, owner, \&format!("/{}/deliveries", encode_url_component(id)))?).await
    }
}
