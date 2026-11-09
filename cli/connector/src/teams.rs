// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

use crate::api::{
    MakeTeamRequest, PatchTeamRequest, PatchWorkerRequest, PendingInvitationItem,
    RegisterWorkerRequest, RegisteredTeamWorker, RemoveTeamMemberRequest, RevokeInvitationRequest,
    TeamDemand, TeamGrants, TeamMemberItem, TeamMemberRequest, TeamRequestItem, TeamResponse,
    TeamSummary, TeamWorkerEntry,
};
use crate::{Client, ConnectorError, http};
use reqwest::{Method, RequestBuilder};
use serde::de::DeserializeOwned;

pub struct TeamsApi<'a>(pub(crate) &'a Client);

impl TeamsApi<'_> {
    fn request(&self, method: Method, endpoint: &str) -> Result<RequestBuilder, ConnectorError> {
        http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            method,
            endpoint,
            true,
        )
    }

    async fn send<T: DeserializeOwned>(req: RequestBuilder) -> Result<T, ConnectorError> {
        http::decode(req.send().await?).await
    }

    pub async fn list(&self) -> Result<Vec<TeamSummary>, ConnectorError> {
        Self::send(self.request(Method::GET, "teams")?).await
    }

    pub async fn create(&self, body: MakeTeamRequest) -> Result<String, ConnectorError> {
        Self::send(self.request(Method::PUT, "teams")?.json(&body)).await
    }

    pub async fn get(&self, team: &str) -> Result<TeamResponse, ConnectorError> {
        Self::send(self.request(Method::GET, &format!("teams/{team}"))?).await
    }

    pub async fn update(
        &self,
        team: &str,
        body: PatchTeamRequest,
    ) -> Result<String, ConnectorError> {
        Self::send(
            self.request(Method::PATCH, &format!("teams/{team}"))?
                .json(&body),
        )
        .await
    }

    pub async fn delete(&self, team: &str) -> Result<String, ConnectorError> {
        Self::send(self.request(Method::DELETE, &format!("teams/{team}"))?).await
    }

    pub async fn list_members(&self, team: &str) -> Result<Vec<TeamMemberItem>, ConnectorError> {
        Self::send(self.request(Method::GET, &format!("teams/{team}/members"))?).await
    }

    pub async fn add_user(
        &self,
        team: &str,
        body: TeamMemberRequest,
    ) -> Result<String, ConnectorError> {
        Self::send(
            self.request(Method::POST, &format!("teams/{team}/members"))?
                .json(&body),
        )
        .await
    }

    pub async fn update_team_member_role(
        &self,
        team: &str,
        body: TeamMemberRequest,
    ) -> Result<String, ConnectorError> {
        Self::send(
            self.request(Method::PATCH, &format!("teams/{team}/members"))?
                .json(&body),
        )
        .await
    }

    pub async fn remove_team_member(
        &self,
        team: &str,
        body: RemoveTeamMemberRequest,
    ) -> Result<String, ConnectorError> {
        Self::send(
            self.request(Method::DELETE, &format!("teams/{team}/members"))?
                .json(&body),
        )
        .await
    }

    pub async fn list_pending_invitations(
        &self,
        team: &str,
    ) -> Result<Vec<PendingInvitationItem>, ConnectorError> {
        Self::send(self.request(Method::GET, &format!("teams/{team}/invitations"))?).await
    }

    pub async fn withdraw_team_invitation(
        &self,
        team: &str,
        body: RevokeInvitationRequest,
    ) -> Result<String, ConnectorError> {
        Self::send(
            self.request(Method::DELETE, &format!("teams/{team}/invitations"))?
                .json(&body),
        )
        .await
    }

    pub async fn list_team_grant_requests(
        &self,
        team: &str,
    ) -> Result<Vec<TeamRequestItem>, ConnectorError> {
        Self::send(self.request(Method::GET, &format!("teams/{team}/requests"))?).await
    }

    pub async fn approve_grant_request(
        &self,
        team: &str,
        request_id: &str,
    ) -> Result<String, ConnectorError> {
        Self::send(self.request(Method::POST, &format!("teams/{team}/requests/{request_id}"))?)
            .await
    }

    pub async fn reject_team_grant_request(
        &self,
        team: &str,
        request_id: &str,
    ) -> Result<String, ConnectorError> {
        Self::send(self.request(
            Method::DELETE,
            &format!("teams/{team}/requests/{request_id}"),
        )?)
        .await
    }

    pub async fn list_projects_and_caches_granting_team(
        &self,
        team: &str,
    ) -> Result<TeamGrants, ConnectorError> {
        Self::send(self.request(Method::GET, &format!("teams/{team}/grants"))?).await
    }

    pub async fn list_team_workers(
        &self,
        team: &str,
    ) -> Result<Vec<TeamWorkerEntry>, ConnectorError> {
        Self::send(self.request(Method::GET, &format!("teams/{team}/workers"))?).await
    }

    pub async fn register_team_worker(
        &self,
        team: &str,
        body: RegisterWorkerRequest,
    ) -> Result<RegisteredTeamWorker, ConnectorError> {
        Self::send(
            self.request(Method::POST, &format!("teams/{team}/workers"))?
                .json(&body),
        )
        .await
    }

    pub async fn update_team_worker(
        &self,
        team: &str,
        worker_id: &str,
        body: PatchWorkerRequest,
    ) -> Result<String, ConnectorError> {
        Self::send(
            self.request(Method::PATCH, &format!("teams/{team}/workers/{worker_id}"))?
                .json(&body),
        )
        .await
    }

    pub async fn delete_team_worker(
        &self,
        team: &str,
        worker_id: &str,
    ) -> Result<String, ConnectorError> {
        Self::send(self.request(Method::DELETE, &format!("teams/{team}/workers/{worker_id}"))?)
            .await
    }

    pub async fn get_team_workers_demand_missing_capabilities(
        &self,
        team: &str,
        projects: Option<&str>,
    ) -> Result<TeamDemand, ConnectorError> {
        let mut req = self.request(Method::GET, &format!("teams/{team}/workers/demand"))?;
        if let Some(projects) = projects {
            req = req.query(&[("projects", projects)]);
        }
        Self::send(req).await
    }
}
