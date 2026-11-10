// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

use crate::api::{
    ApiKeyInfo, ApiKeyPermissionsResponse, ApiKeyRequest, AuditLogEntry, CreateApiKeyRequest,
    CreateSshKeyRequest, DeleteUserRequest, GetUserSettingsResponse, InviteItem,
    InviteTokenRequest, PaginatedSessionInfo, PatchApiKeyRequest, PatchUserSettingsRequest,
    SshKeyInfo, UserInfoResponse,
};
use crate::http::encode_url_component;
use crate::{Client, ConnectorError, http};
use reqwest::Method;

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
        http::decode::<Option<UserInfoResponse>>(req.send().await?)
            .await?
            .ok_or(ConnectorError::Unauthorized)
    }

    pub async fn delete_own_account(
        &self,
        body: DeleteUserRequest,
    ) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::DELETE,
            "user",
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn get_user_settings(&self) -> Result<GetUserSettingsResponse, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            "user/settings",
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

    pub async fn create_key(&self, body: CreateApiKeyRequest) -> Result<String, ConnectorError> {
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

    pub async fn edit_api_key(
        &self,
        api_id: &str,
        body: PatchApiKeyRequest,
    ) -> Result<ApiKeyInfo, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::PATCH,
            &format!("user/keys/{}", encode_url_component(api_id)),
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn delete_api_key_by_name(&self, name: &str) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::DELETE,
            "user/keys",
            true,
        )?
        .json(&ApiKeyRequest {
            name: name.to_owned(),
        });
        http::decode(req.send().await?).await
    }

    pub async fn key_permissions(&self) -> Result<ApiKeyPermissionsResponse, ConnectorError> {
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

    pub async fn revoke_key(&self, api_id: &str) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            &format!("user/keys/{}/revoke", encode_url_component(api_id)),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn get_ssh_keys(&self) -> Result<Vec<SshKeyInfo>, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            "user/ssh-keys",
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn add_user_ssh_key(
        &self,
        body: CreateSshKeyRequest,
    ) -> Result<SshKeyInfo, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            "user/ssh-keys",
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn delete_user_ssh_key(&self, ssh_key_id: &str) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::DELETE,
            &format!("user/ssh-keys/{}", encode_url_component(ssh_key_id)),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn list_sessions(
        &self,
        page: Option<u64>,
        per_page: Option<u64>,
    ) -> Result<PaginatedSessionInfo, ConnectorError> {
        let mut req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            "user/sessions",
            true,
        )?;
        if let Some(page) = page {
            req = req.query(&[("page", page)]);
        }
        if let Some(per_page) = per_page {
            req = req.query(&[("per_page", per_page)]);
        }
        http::decode(req.send().await?).await
    }

    pub async fn delete_session(&self, session_id: &str) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::DELETE,
            &format!("user/sessions/{}", encode_url_component(session_id)),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn list_invitations_addressed_to_me(
        &self,
    ) -> Result<Vec<InviteItem>, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            "user/invites",
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn accept_user_invite(&self, token: &str) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            "user/invites/accept",
            true,
        )?
        .json(&InviteTokenRequest {
            token: token.to_owned(),
        });
        http::decode(req.send().await?).await
    }

    pub async fn decline_user_invite(&self, token: &str) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            "user/invites/decline",
            true,
        )?
        .json(&InviteTokenRequest {
            token: token.to_owned(),
        });
        http::decode(req.send().await?).await
    }

    pub async fn disconnect_user_linked_login_provider(
        &self,
        provider: &str,
    ) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::DELETE,
            &format!("user/identities/{}", encode_url_component(provider)),
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
}
