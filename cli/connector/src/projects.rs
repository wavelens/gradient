use crate::{Client, ConnectorError, ListResponse, PaginatedListResponse, http};
use reqwest::Method;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ProjectResponse {
    pub id: String,
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub public_key: String,
    pub created_by: String,
    pub created_at: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MakeProjectRequest {
    pub name: String,
    pub display_name: String,
    pub description: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct PatchProjectRequest {
    pub name: Option<String>,
    pub display_name: Option<String>,
    pub description: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AddUserRequest {
    pub user: String,
    pub role: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RemoveUserRequest {
    pub user: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Role {
    pub id: String,
    pub name: String,
    pub permissions: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MakeRoleRequest {
    pub name: String,
    pub permissions: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct PatchRoleRequest {
    pub name: Option<String>,
    pub permissions: Option<Vec<String>>,
}

pub struct ProjectsApi<'a>(pub(crate) &'a Client);

impl ProjectsApi<'_> {
    pub async fn list(&self) -> Result<PaginatedListResponse, ConnectorError> {
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

    pub async fn available(&self) -> Result<PaginatedListResponse, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            "projects/available",
            true,
        )?;
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

    pub async fn users(&self, project: &str) -> Result<ListResponse, ConnectorError> {
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
        body: AddUserRequest,
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
        body: AddUserRequest,
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
        body: RemoveUserRequest,
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

    pub async fn roles(&self, project: &str) -> Result<Vec<Role>, ConnectorError> {
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
        body: MakeRoleRequest,
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

    pub async fn get_role(&self, project: &str, role_id: &str) -> Result<Role, ConnectorError> {
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

    pub async fn subscriptions(&self, project: &str) -> Result<ListResponse, ConnectorError> {
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
