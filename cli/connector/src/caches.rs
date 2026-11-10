// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

use crate::api::{
    AddMemberRequest, AddUpstreamRequest, CacheResponse, CacheStatsResponse,
    CacheSubscribersResponse, CacheUpstreamId, ChangeMemberRoleRequest, ChunkReceived,
    CreateRoleRequest, CreatedEntityResponse, GrantCacheRequest, MakeCacheRequest, MemberItem,
    NarDetail, NarStats,
    NarinfoPart, PaginatedCache, PaginatedNarSummary, PatchCacheGrantRequest, PatchCacheRequest,
    PatchRoleRequest, PatchUpstreamRequest, PendingInvitationItem, RemoveMemberRequest,
    RevokeInvitationRequest, RoleListResponseCacheRoleOwner, RoleResponseCacheRoleOwner,
    SubscriptionRequestItem, TeamGrantEntry, UpstreamCacheItem, UpstreamTestResponse,
};
use crate::http::encode_url_component;
use crate::{Client, ConnectorError, http};
use reqwest::Method;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct NarListQuery {
    pub hash: Option<String>,
    pub package: Option<String>,
    pub sort: Option<String>,
    pub order: Option<String>,
    pub page: Option<u32>,
    pub per_page: Option<u32>,
}

pub struct CachesApi<'a>(pub(crate) &'a Client);

impl CachesApi<'_> {
    pub async fn list(&self) -> Result<PaginatedCache, ConnectorError> {
        let (items, total) = self.0.fetch_all_pages_and_total_count("caches").await?;
        Ok(PaginatedCache {
            per_page: i64::try_from(items.len()).unwrap_or(i64::MAX),
            page: 1,
            total,
            items,
        })
    }

    pub async fn create(
        &self,
        body: MakeCacheRequest,
    ) -> Result<CreatedEntityResponse, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::PUT,
            "caches",
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn available(&self, name: &str) -> Result<bool, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            "caches/available",
            true,
        )?
        .query(&[("name", name)]);
        http::decode(req.send().await?).await
    }

    pub async fn get(&self, cache: &str) -> Result<CacheResponse, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("caches/{}", encode_url_component(cache)),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn update(
        &self,
        cache: &str,
        body: PatchCacheRequest,
    ) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::PATCH,
            &format!("caches/{}", encode_url_component(cache)),
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn delete(&self, cache: &str) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::DELETE,
            &format!("caches/{}", encode_url_component(cache)),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn key(&self, cache: &str) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("caches/{}/key", encode_url_component(cache)),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn public_key(&self, cache: &str) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("caches/{}/public-key", encode_url_component(cache)),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn stats(&self, cache: &str) -> Result<CacheStatsResponse, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("caches/{}/stats", encode_url_component(cache)),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn upstream_caches(
        &self,
        cache: &str,
    ) -> Result<Vec<UpstreamCacheItem>, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("caches/{}/upstream-caches", encode_url_component(cache)),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn add_upstream_cache(
        &self,
        cache: &str,
        body: AddUpstreamRequest,
    ) -> Result<CacheUpstreamId, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::PUT,
            &format!("caches/{}/upstream-caches", encode_url_component(cache)),
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn update_upstream_cache(
        &self,
        cache: &str,
        id: &str,
        body: PatchUpstreamRequest,
    ) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::PATCH,
            &format!(
                "caches/{}/upstream-caches/{}",
                encode_url_component(cache),
                encode_url_component(id)
            ),
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn delete_upstream_cache(
        &self,
        cache: &str,
        id: &str,
    ) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::DELETE,
            &format!(
                "caches/{}/upstream-caches/{}",
                encode_url_component(cache),
                encode_url_component(id)
            ),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn test_upstream_cache(
        &self,
        cache: &str,
        id: &str,
    ) -> Result<UpstreamTestResponse, ConnectorError> {
        self.0
            .send_authenticated_request(
                Method::POST,
                &format!(
                    "caches/{}/upstream-caches/{}/test",
                    encode_url_component(cache),
                    encode_url_component(id)
                ),
                None::<&()>,
            )
            .await
    }

    pub async fn activate_cache(&self, cache: &str) -> Result<String, ConnectorError> {
        self.0
            .send_authenticated_request(
                Method::POST,
                &format!("caches/{}/active", encode_url_component(cache)),
                None::<&()>,
            )
            .await
    }

    pub async fn deactivate_cache(&self, cache: &str) -> Result<String, ConnectorError> {
        self.0
            .send_authenticated_request(
                Method::DELETE,
                &format!("caches/{}/active", encode_url_component(cache)),
                None::<&()>,
            )
            .await
    }

    pub async fn make_cache_public(&self, cache: &str) -> Result<String, ConnectorError> {
        self.0
            .send_authenticated_request(
                Method::POST,
                &format!("caches/{}/public", encode_url_component(cache)),
                None::<&()>,
            )
            .await
    }

    pub async fn make_cache_private(&self, cache: &str) -> Result<String, ConnectorError> {
        self.0
            .send_authenticated_request(
                Method::DELETE,
                &format!("caches/{}/public", encode_url_component(cache)),
                None::<&()>,
            )
            .await
    }

    pub async fn list_members(&self, cache: &str) -> Result<Vec<MemberItem>, ConnectorError> {
        self.0
            .send_authenticated_request(
                Method::GET,
                &format!("caches/{}/members", encode_url_component(cache)),
                None::<&()>,
            )
            .await
    }

    pub async fn invite_cache_member(
        &self,
        cache: &str,
        body: AddMemberRequest,
    ) -> Result<String, ConnectorError> {
        self.0
            .send_authenticated_request(
                Method::POST,
                &format!("caches/{}/members", encode_url_component(cache)),
                Some(&body),
            )
            .await
    }

    pub async fn update_cache_member_role(
        &self,
        cache: &str,
        body: ChangeMemberRoleRequest,
    ) -> Result<String, ConnectorError> {
        self.0
            .send_authenticated_request(
                Method::PATCH,
                &format!("caches/{}/members", encode_url_component(cache)),
                Some(&body),
            )
            .await
    }

    pub async fn delete_cache_member(
        &self,
        cache: &str,
        body: RemoveMemberRequest,
    ) -> Result<String, ConnectorError> {
        self.0
            .send_authenticated_request(
                Method::DELETE,
                &format!("caches/{}/members", encode_url_component(cache)),
                Some(&body),
            )
            .await
    }

    pub async fn list_pending_invitations(
        &self,
        cache: &str,
    ) -> Result<Vec<PendingInvitationItem>, ConnectorError> {
        self.0
            .send_authenticated_request(
                Method::GET,
                &format!("caches/{}/invitations", encode_url_component(cache)),
                None::<&()>,
            )
            .await
    }

    pub async fn revoke_pending_invitation(
        &self,
        cache: &str,
        body: RevokeInvitationRequest,
    ) -> Result<String, ConnectorError> {
        self.0
            .send_authenticated_request(
                Method::DELETE,
                &format!("caches/{}/invitations", encode_url_component(cache)),
                Some(&body),
            )
            .await
    }

    pub async fn list_roles_and_available_permissions(
        &self,
        cache: &str,
    ) -> Result<RoleListResponseCacheRoleOwner, ConnectorError> {
        self.0
            .send_authenticated_request(
                Method::GET,
                &format!("caches/{}/roles", encode_url_component(cache)),
                None::<&()>,
            )
            .await
    }

    pub async fn fetch_cache_role(
        &self,
        cache: &str,
        role: &str,
    ) -> Result<RoleResponseCacheRoleOwner, ConnectorError> {
        self.0
            .send_authenticated_request(
                Method::GET,
                &format!(
                    "caches/{}/roles/{}",
                    encode_url_component(cache),
                    encode_url_component(role)
                ),
                None::<&()>,
            )
            .await
    }

    pub async fn create_custom_role(
        &self,
        cache: &str,
        body: CreateRoleRequest,
    ) -> Result<RoleResponseCacheRoleOwner, ConnectorError> {
        self.0
            .send_authenticated_request(
                Method::POST,
                &format!("caches/{}/roles", encode_url_component(cache)),
                Some(&body),
            )
            .await
    }

    pub async fn update_custom_role(
        &self,
        cache: &str,
        role: &str,
        body: PatchRoleRequest,
    ) -> Result<RoleResponseCacheRoleOwner, ConnectorError> {
        self.0
            .send_authenticated_request(
                Method::PATCH,
                &format!(
                    "caches/{}/roles/{}",
                    encode_url_component(cache),
                    encode_url_component(role)
                ),
                Some(&body),
            )
            .await
    }

    pub async fn delete_custom_role(
        &self,
        cache: &str,
        role: &str,
    ) -> Result<bool, ConnectorError> {
        self.0
            .send_authenticated_request(
                Method::DELETE,
                &format!(
                    "caches/{}/roles/{}",
                    encode_url_component(cache),
                    encode_url_component(role)
                ),
                None::<&()>,
            )
            .await
    }

    pub async fn list_teams_granted_access_to_cache(
        &self,
        cache: &str,
    ) -> Result<Vec<TeamGrantEntry>, ConnectorError> {
        self.0
            .send_authenticated_request(
                Method::GET,
                &format!("caches/{}/teams", encode_url_component(cache)),
                None::<&()>,
            )
            .await
    }

    pub async fn grant_team_access_to_cache(
        &self,
        cache: &str,
        body: GrantCacheRequest,
    ) -> Result<String, ConnectorError> {
        self.0
            .send_authenticated_request(
                Method::POST,
                &format!("caches/{}/teams", encode_url_component(cache)),
                Some(&body),
            )
            .await
    }

    pub async fn update_cache_team_grant_role(
        &self,
        cache: &str,
        team: &str,
        body: PatchCacheGrantRequest,
    ) -> Result<String, ConnectorError> {
        self.0
            .send_authenticated_request(
                Method::PATCH,
                &format!(
                    "caches/{}/teams/{}",
                    encode_url_component(cache),
                    encode_url_component(team)
                ),
                Some(&body),
            )
            .await
    }

    pub async fn remove_team_grant(
        &self,
        cache: &str,
        team: &str,
    ) -> Result<String, ConnectorError> {
        self.0
            .send_authenticated_request(
                Method::DELETE,
                &format!(
                    "caches/{}/teams/{}",
                    encode_url_component(cache),
                    encode_url_component(team)
                ),
                None::<&()>,
            )
            .await
    }

    pub async fn list_projects_subscribed_to_cache(
        &self,
        cache: &str,
    ) -> Result<CacheSubscribersResponse, ConnectorError> {
        self.0
            .send_authenticated_request(
                Method::GET,
                &format!("caches/{}/subscribers", encode_url_component(cache)),
                None::<&()>,
            )
            .await
    }

    pub async fn list_subscription_requests(
        &self,
        cache: &str,
    ) -> Result<Vec<SubscriptionRequestItem>, ConnectorError> {
        self.0
            .send_authenticated_request(
                Method::GET,
                &format!(
                    "caches/{}/subscription-requests",
                    encode_url_component(cache)
                ),
                None::<&()>,
            )
            .await
    }

    pub async fn approve_subscription_request(
        &self,
        cache: &str,
        project: &str,
    ) -> Result<String, ConnectorError> {
        self.0
            .send_authenticated_request(
                Method::POST,
                &format!(
                    "caches/{}/subscription-requests/{}",
                    encode_url_component(cache),
                    encode_url_component(project)
                ),
                None::<&()>,
            )
            .await
    }

    pub async fn deny_subscription_request(
        &self,
        cache: &str,
        project: &str,
    ) -> Result<String, ConnectorError> {
        self.0
            .send_authenticated_request(
                Method::DELETE,
                &format!(
                    "caches/{}/subscription-requests/{}",
                    encode_url_component(cache),
                    encode_url_component(project)
                ),
                None::<&()>,
            )
            .await
    }

    pub async fn nars_list(
        &self,
        cache: &str,
        q: NarListQuery,
    ) -> Result<PaginatedNarSummary, ConnectorError> {
        let mut qs: Vec<(&str, String)> = Vec::new();
        if let Some(v) = q.hash {
            qs.push(("hash", v));
        }
        if let Some(v) = q.package {
            qs.push(("package", v));
        }
        if let Some(v) = q.sort {
            qs.push(("sort", v));
        }
        if let Some(v) = q.order {
            qs.push(("order", v));
        }
        if let Some(v) = q.page {
            qs.push(("page", v.to_string()));
        }
        if let Some(v) = q.per_page {
            qs.push(("per_page", v.to_string()));
        }
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("caches/{}/nars", encode_url_component(cache)),
            true,
        )?
        .query(&qs);
        http::decode(req.send().await?).await
    }

    pub async fn nar_show(&self, cache: &str, hash: &str) -> Result<NarDetail, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!(
                "caches/{}/nars/{}",
                encode_url_component(cache),
                encode_url_component(hash)
            ),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn nars_stats(&self, cache: &str) -> Result<NarStats, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("caches/{}/nars/stats", encode_url_component(cache)),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    /// The returned staged byte count is authoritative. The caller must set its next offset to it
    /// and resend when it did not advance.
    pub async fn nar_upload_chunk(
        &self,
        cache: &str,
        store_hash: &str,
        offset: u64,
        chunk: bytes::Bytes,
    ) -> Result<u64, ConnectorError> {
        let timer = self.0.transfer_no_progress_timer();
        let resp = http::send_upload(&timer, || {
            Ok(http::request(
                self.0.stream_http(),
                self.0.base_url(),
                self.0.token(),
                Method::PUT,
                &format!(
                    "caches/{}/nars/{}/chunk?offset={}",
                    encode_url_component(cache),
                    encode_url_component(store_hash),
                    encode_url_component(offset)
                ),
                true,
            )?
            .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
            .header(reqwest::header::CONTENT_LENGTH, chunk.len())
            .body(timer.request_body_stream_from_bytes_recording_progress(chunk.clone())))
        })
        .await?;
        let received: ChunkReceived = http::decode(resp).await?;
        Ok(received.received.unsigned_abs())
    }

    pub async fn nar_upload_finalize(
        &self,
        cache: &str,
        store_hash: &str,
        narinfo: NarinfoPart,
    ) -> Result<(), ConnectorError> {
        let resp = http::send_upload(&self.0.transfer_no_progress_timer(), || {
            Ok(http::request(
                self.0.http(),
                self.0.base_url(),
                self.0.token(),
                Method::POST,
                &format!(
                    "caches/{}/nars/{}/finalize",
                    encode_url_component(cache),
                    encode_url_component(store_hash)
                ),
                true,
            )?
            .json(&narinfo))
        })
        .await?;
        let _: serde_json::Value = http::decode(resp).await?;
        Ok(())
    }

    pub async fn nar_delete(&self, cache: &str, hash: &str) -> Result<(), ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::DELETE,
            &format!(
                "caches/{}/nars/{}",
                encode_url_component(cache),
                encode_url_component(hash)
            ),
            true,
        )?;
        let resp = req.send().await?;
        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(ConnectorError::Unauthorized);
        }
        if !status.is_success() {
            let bytes = resp.bytes().await?;
            let message = if let Ok(env) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                env.get("message")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned)
                    .unwrap_or_else(|| String::from_utf8_lossy(&bytes).into_owned())
            } else {
                String::from_utf8_lossy(&bytes).into_owned()
            };
            return Err(ConnectorError::Api { status, message });
        }
        Ok(())
    }
}
