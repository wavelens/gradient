use crate::{Client, ConnectorError, http};
use reqwest::Method;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct UserResponse {
    pub id: String,
    pub username: String,
    pub name: String,
    pub email: String,
    pub superuser: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct UserSettings {
    pub username: String,
    pub name: String,
    pub email: String,
    pub is_oidc: bool,
    pub managed: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct PatchUserSettingsRequest {
    pub username: Option<String>,
    pub name: Option<String>,
    pub email: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ApiKey {
    pub id: String,
    pub name: String,
    pub managed: bool,
    pub permissions: Vec<String>,
    pub organization: Option<String>,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub expires_at: Option<String>,
    pub revoked_at: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MakeApiKeyRequest {
    pub name: String,
    pub permissions: Vec<String>,
    pub expires_in_days: Option<u32>,
    pub organization: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CreatedApiKey {
    pub id: String,
    pub name: String,
    pub token: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Session {
    pub id: String,
    pub user_agent: Option<String>,
    pub ip: Option<String>,
    pub created_at: String,
    pub last_used_at: String,
    pub expires_at: String,
    pub remember_me: bool,
    pub current: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AuditEntry {
    pub id: String,
    pub event: String,
    pub ip: Option<String>,
    pub user_agent: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub created_at: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct UserSearchResult {
    pub id: String,
    pub username: String,
    pub name: String,
}

pub struct UserApi<'a>(pub(crate) &'a Client);

impl UserApi<'_> {
    pub async fn get(&self) -> Result<UserResponse, ConnectorError> {
        let req = http::request(self.0.http(), self.0.base_url(), self.0.token(), Method::GET, "user", true)?;
        http::decode(req.send().await?).await
    }

    pub async fn update_settings(&self, body: PatchUserSettingsRequest) -> Result<String, ConnectorError> {
        let req = http::request(self.0.http(), self.0.base_url(), self.0.token(), Method::PATCH, "user/settings", true)?
            .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn keys(&self) -> Result<Vec<ApiKey>, ConnectorError> {
        let req = http::request(self.0.http(), self.0.base_url(), self.0.token(), Method::GET, "user/keys", true)?;
        http::decode(req.send().await?).await
    }

    pub async fn create_key(&self, body: MakeApiKeyRequest) -> Result<CreatedApiKey, ConnectorError> {
        let req = http::request(self.0.http(), self.0.base_url(), self.0.token(), Method::POST, "user/keys", true)?
            .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn key_permissions(&self) -> Result<Vec<String>, ConnectorError> {
        let req = http::request(self.0.http(), self.0.base_url(), self.0.token(), Method::GET, "user/keys/permissions", true)?;
        http::decode(req.send().await?).await
    }

    pub async fn get_key(&self, api_id: &str) -> Result<ApiKey, ConnectorError> {
        let req = http::request(self.0.http(), self.0.base_url(), self.0.token(), Method::GET, &format!("user/keys/{api_id}"), true)?;
        http::decode(req.send().await?).await
    }

    pub async fn delete_key(&self, api_id: &str) -> Result<String, ConnectorError> {
        let req = http::request(self.0.http(), self.0.base_url(), self.0.token(), Method::DELETE, &format!("user/keys/{api_id}"), true)?;
        http::decode(req.send().await?).await
    }

    pub async fn revoke_key(&self, api_id: &str) -> Result<String, ConnectorError> {
        let req = http::request(self.0.http(), self.0.base_url(), self.0.token(), Method::POST, &format!("user/keys/{api_id}/revoke"), true)?;
        http::decode(req.send().await?).await
    }

    pub async fn sessions(&self) -> Result<Vec<Session>, ConnectorError> {
        let req = http::request(self.0.http(), self.0.base_url(), self.0.token(), Method::GET, "user/sessions", true)?;
        http::decode(req.send().await?).await
    }

    pub async fn delete_session(&self, session_id: &str) -> Result<String, ConnectorError> {
        let req = http::request(self.0.http(), self.0.base_url(), self.0.token(), Method::DELETE, &format!("user/sessions/{session_id}"), true)?;
        http::decode(req.send().await?).await
    }

    pub async fn audit_log(&self) -> Result<Vec<AuditEntry>, ConnectorError> {
        let req = http::request(self.0.http(), self.0.base_url(), self.0.token(), Method::GET, "user/audit-log", true)?;
        http::decode(req.send().await?).await
    }

    pub async fn search(&self, query: &str) -> Result<Vec<UserSearchResult>, ConnectorError> {
        let req = http::request(self.0.http(), self.0.base_url(), self.0.token(), Method::GET, &format!("user/search?q={query}"), true)?;
        http::decode(req.send().await?).await
    }
}
