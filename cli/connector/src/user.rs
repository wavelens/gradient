// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

use crate::api::{
    ApiKeyInfo, AuditLogEntry, CreateApiKeyRequest, PatchUserSettingsRequest, SessionInfo,
    UserInfoResponse, UserSearchResult,
};
use crate::{Client, ConnectorError, http};
use reqwest::Method;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CreatedApiKey {
    pub id: String,
    pub name: String,
    pub token: String,
}

pub struct UserApi<'a>(pub(crate) &'a Client);

impl UserApi<'_> {
    pub async fn get(&self) -> Result<UserInfoResponse, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            "user",
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn update_settings(
        &self,
        body: PatchUserSettingsRequest,
    ) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::PATCH,
            "user/settings",
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn keys(&self) -> Result<Vec<ApiKeyInfo>, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            "user/keys",
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn create_key(
        &self,
        body: CreateApiKeyRequest,
    ) -> Result<CreatedApiKey, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            "user/keys",
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn key_permissions(&self) -> Result<Vec<String>, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            "user/keys/permissions",
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn get_key(&self, api_id: &str) -> Result<ApiKeyInfo, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("user/keys/{api_id}"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn delete_key(&self, api_id: &str) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::DELETE,
            &format!("user/keys/{api_id}"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn revoke_key(&self, api_id: &str) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            &format!("user/keys/{api_id}/revoke"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn sessions(&self) -> Result<Vec<SessionInfo>, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            "user/sessions",
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn delete_session(&self, session_id: &str) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::DELETE,
            &format!("user/sessions/{session_id}"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn audit_log(&self) -> Result<Vec<AuditLogEntry>, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            "user/audit-log",
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn search(&self, query: &str) -> Result<Vec<UserSearchResult>, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("user/search?q={query}"),
            true,
        )?;
        http::decode(req.send().await?).await
    }
}
