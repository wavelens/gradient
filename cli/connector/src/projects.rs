// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

use crate::api::{
    AddMemberRequest, CacheSubscriptionItem, CreateRoleRequest, MakeProjectRequest, MemberItem,
    PaginatedProjectSummary, PatchProjectRequest, PatchRoleRequest, ProjectResponse,
    RemoveMemberRequest, RoleResponseProjectRoleOwner,
};
use crate::{Client, ConnectorError, http};
use reqwest::Method;

pub struct ProjectsApi<'a>(pub(crate) &'a Client);

impl ProjectsApi<'_> {
    pub async fn list(&self) -> Result<PaginatedProjectSummary, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            "projects",
            true,
        )?;
        http::decode(req.send().await?).await
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

    pub async fn create(&self, body: MakeProjectRequest) -> Result<String, ConnectorError> {
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
            &format!("projects/{project}"),
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
            &format!("projects/{project}"),
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
            &format!("projects/{project}"),
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
            &format!("projects/{project}/users"),
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
            &format!("projects/{project}/users"),
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn update_user(
        &self,
        project: &str,
        body: AddMemberRequest,
    ) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::PATCH,
            &format!("projects/{project}/users"),
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
            &format!("projects/{project}/users"),
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
            &format!("projects/{project}/roles"),
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
            &format!("projects/{project}/roles"),
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
            &format!("projects/{project}/roles/{role_id}"),
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
            &format!("projects/{project}/roles/{role_id}"),
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
            &format!("projects/{project}/roles/{role_id}"),
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
            &format!("projects/{project}/ssh"),
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
            &format!("projects/{project}/ssh"),
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
            &format!("projects/{project}/subscribe"),
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
            &format!("projects/{project}/subscribe/{cache}"),
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
            &format!("projects/{project}/subscribe/{cache}"),
            true,
        )?;
        http::decode(req.send().await?).await
    }
}
