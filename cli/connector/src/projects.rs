// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

use crate::api::{
    AddMemberRequest, CacheSubscriptionItem, ChangeMemberRoleRequest, CreateRoleRequest,
    CreatedEntityResponse,
    GrantProjectRequest, MakeProjectRequest, MemberItem, PaginatedProjectSummary,
    PatchProjectGrantRequest, PatchProjectRequest, PatchRoleRequest, PendingInvitationItem,
    PersonWithAccessToProjectOrCache, ProjectResponse, ProjectSecretSummary,
    PutProjectSecretRequest, RemoveMemberRequest, RevokeInvitationRequest,
    RoleResponseProjectRoleOwner, SopsRecipientResponse, TeamGrantEntry,
};
use crate::http::encode_url_component;
use crate::{Client, ConnectorError, http};
use reqwest::Method;

pub struct ProjectsApi<'a>(pub(crate) &'a Client);

impl ProjectsApi<'_> {
    pub async fn list(&self) -> Result<PaginatedProjectSummary, ConnectorError> {
        let (items, total) = self.0.fetch_all_pages_and_total_count("projects").await?;
        Ok(PaginatedProjectSummary {
            per_page: i64::try_from(items.len()).unwrap_or(i64::MAX),
            page: 1,
            total,
            items,
        })
    }

    pub async fn available(&self, name: &str) -> Result<bool, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            "projects/available",
            true,
        )?
        .query(&[("name", name)]);
        http::decode(req.send().await?).await
    }

    pub async fn create(
        &self,
        body: MakeProjectRequest,
    ) -> Result<CreatedEntityResponse, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::PUT,
            "projects",
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn get(&self, project: &str) -> Result<ProjectResponse, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("projects/{}", encode_url_component(project)),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn update(
        &self,
        project: &str,
        body: PatchProjectRequest,
    ) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::PATCH,
            &format!("projects/{}", encode_url_component(project)),
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn delete(&self, project: &str) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::DELETE,
            &format!("projects/{}", encode_url_component(project)),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn users(&self, project: &str) -> Result<Vec<MemberItem>, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("projects/{}/users", encode_url_component(project)),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn list_project_access(
        &self,
        project: &str,
    ) -> Result<Vec<PersonWithAccessToProjectOrCache>, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("projects/{project}/access"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn add_user(
        &self,
        project: &str,
        body: AddMemberRequest,
    ) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            &format!("projects/{}/users", encode_url_component(project)),
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn update_user(
        &self,
        project: &str,
        body: ChangeMemberRoleRequest,
    ) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::PATCH,
            &format!("projects/{}/users", encode_url_component(project)),
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn remove_user(
        &self,
        project: &str,
        body: RemoveMemberRequest,
    ) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::DELETE,
            &format!("projects/{}/users", encode_url_component(project)),
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn roles(
        &self,
        project: &str,
    ) -> Result<Vec<RoleResponseProjectRoleOwner>, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("projects/{}/roles", encode_url_component(project)),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn create_role(
        &self,
        project: &str,
        body: CreateRoleRequest,
    ) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            &format!("projects/{}/roles", encode_url_component(project)),
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn get_role(
        &self,
        project: &str,
        role_id: &str,
    ) -> Result<RoleResponseProjectRoleOwner, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!(
                "projects/{}/roles/{}",
                encode_url_component(project),
                encode_url_component(role_id)
            ),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn update_role(
        &self,
        project: &str,
        role_id: &str,
        body: PatchRoleRequest,
    ) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::PATCH,
            &format!(
                "projects/{}/roles/{}",
                encode_url_component(project),
                encode_url_component(role_id)
            ),
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn delete_role(
        &self,
        project: &str,
        role_id: &str,
    ) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::DELETE,
            &format!(
                "projects/{}/roles/{}",
                encode_url_component(project),
                encode_url_component(role_id)
            ),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn ssh_key(&self, project: &str) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("projects/{}/ssh", encode_url_component(project)),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn regenerate_ssh(&self, project: &str) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            &format!("projects/{}/ssh", encode_url_component(project)),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn subscriptions(
        &self,
        project: &str,
    ) -> Result<Vec<CacheSubscriptionItem>, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("projects/{}/subscribe", encode_url_component(project)),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn subscribe(&self, project: &str, cache: &str) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            &format!(
                "projects/{}/subscribe/{}",
                encode_url_component(project),
                encode_url_component(cache)
            ),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn unsubscribe(&self, project: &str, cache: &str) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::DELETE,
            &format!(
                "projects/{}/subscribe/{}",
                encode_url_component(project),
                encode_url_component(cache)
            ),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn list_secret_summaries(
        &self,
        project: &str,
    ) -> Result<Vec<ProjectSecretSummary>, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("projects/{}/secrets", encode_url_component(project)),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn get_project_secret_summary(
        &self,
        project: &str,
        name: &str,
    ) -> Result<ProjectSecretSummary, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!(
                "projects/{}/secrets/{}",
                encode_url_component(project),
                encode_url_component(name)
            ),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn put_project_secret(
        &self,
        project: &str,
        name: &str,
        body: PutProjectSecretRequest,
    ) -> Result<ProjectSecretSummary, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::PUT,
            &format!(
                "projects/{}/secrets/{}",
                encode_url_component(project),
                encode_url_component(name)
            ),
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn delete_project_secret(
        &self,
        project: &str,
        name: &str,
    ) -> Result<bool, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::DELETE,
            &format!(
                "projects/{}/secrets/{}",
                encode_url_component(project),
                encode_url_component(name)
            ),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn get_project_sops_recipient(
        &self,
        project: &str,
    ) -> Result<SopsRecipientResponse, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("projects/{}/sops-recipient", encode_url_component(project)),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn make_project_public(&self, project: &str) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            &format!("projects/{}/public", encode_url_component(project)),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn make_project_private(&self, project: &str) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::DELETE,
            &format!("projects/{}/public", encode_url_component(project)),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn list_project_team_grants(
        &self,
        project: &str,
    ) -> Result<Vec<TeamGrantEntry>, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("projects/{}/teams", encode_url_component(project)),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn grant_team_access_to_project(
        &self,
        project: &str,
        body: GrantProjectRequest,
    ) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            &format!("projects/{}/teams", encode_url_component(project)),
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn update_project_team_grant(
        &self,
        project: &str,
        team: &str,
        body: PatchProjectGrantRequest,
    ) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::PATCH,
            &format!(
                "projects/{}/teams/{}",
                encode_url_component(project),
                encode_url_component(team)
            ),
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn remove_project_team_grant(
        &self,
        project: &str,
        team: &str,
    ) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::DELETE,
            &format!(
                "projects/{}/teams/{}",
                encode_url_component(project),
                encode_url_component(team)
            ),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn list_pending_project_invitations(
        &self,
        project: &str,
    ) -> Result<Vec<PendingInvitationItem>, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("projects/{}/invitations", encode_url_component(project)),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn revoke_project_invitation(
        &self,
        project: &str,
        body: RevokeInvitationRequest,
    ) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::DELETE,
            &format!("projects/{}/invitations", encode_url_component(project)),
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }
}
