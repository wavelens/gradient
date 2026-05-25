use crate::{Client, ConnectorError, ListResponse, PaginatedListResponse, http};
use reqwest::Method;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct OrganizationResponse {
    pub id: String,
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub public_key: String,
    pub created_by: String,
    pub created_at: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MakeOrganizationRequest {
    pub name: String,
    pub display_name: String,
    pub description: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct PatchOrganizationRequest {
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

pub struct OrgsApi<'a>(pub(crate) &'a Client);

impl OrgsApi<'_> {
    pub async fn list(&self) -> Result<PaginatedListResponse, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            "orgs",
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
            "orgs/available",
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn create(&self, body: MakeOrganizationRequest) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::PUT,
            "orgs",
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn get(&self, org: &str) -> Result<OrganizationResponse, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("orgs/{org}"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn update(
        &self,
        org: &str,
        body: PatchOrganizationRequest,
    ) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::PATCH,
            &format!("orgs/{org}"),
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn delete(&self, org: &str) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::DELETE,
            &format!("orgs/{org}"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn users(&self, org: &str) -> Result<ListResponse, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("orgs/{org}/users"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn add_user(
        &self,
        org: &str,
        body: AddUserRequest,
    ) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            &format!("orgs/{org}/users"),
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn update_user(
        &self,
        org: &str,
        body: AddUserRequest,
    ) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::PATCH,
            &format!("orgs/{org}/users"),
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn remove_user(
        &self,
        org: &str,
        body: RemoveUserRequest,
    ) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::DELETE,
            &format!("orgs/{org}/users"),
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn roles(&self, org: &str) -> Result<Vec<Role>, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("orgs/{org}/roles"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn create_role(
        &self,
        org: &str,
        body: MakeRoleRequest,
    ) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            &format!("orgs/{org}/roles"),
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn get_role(&self, org: &str, role_id: &str) -> Result<Role, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("orgs/{org}/roles/{role_id}"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn update_role(
        &self,
        org: &str,
        role_id: &str,
        body: PatchRoleRequest,
    ) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::PATCH,
            &format!("orgs/{org}/roles/{role_id}"),
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn delete_role(&self, org: &str, role_id: &str) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::DELETE,
            &format!("orgs/{org}/roles/{role_id}"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn ssh_key(&self, org: &str) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("orgs/{org}/ssh"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn regenerate_ssh(&self, org: &str) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            &format!("orgs/{org}/ssh"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn subscriptions(&self, org: &str) -> Result<ListResponse, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("orgs/{org}/subscribe"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn subscribe(&self, org: &str, cache: &str) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            &format!("orgs/{org}/subscribe/{cache}"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn unsubscribe(&self, org: &str, cache: &str) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::DELETE,
            &format!("orgs/{org}/subscribe/{cache}"),
            true,
        )?;
        http::decode(req.send().await?).await
    }
}
