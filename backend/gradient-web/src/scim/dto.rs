/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const USER_SCHEMA: &str = "urn:ietf:params:scim:schemas:core:2.0:User";
pub const GROUP_SCHEMA: &str = "urn:ietf:params:scim:schemas:core:2.0:Group";
pub const LIST_SCHEMA: &str = "urn:ietf:params:scim:api:messages:2.0:ListResponse";
pub const PATCH_SCHEMA: &str = "urn:ietf:params:scim:api:messages:2.0:PatchOp";

#[derive(Serialize)]
pub struct ListResponse<T> {
    pub schemas: [&'static str; 1],
    #[serde(rename = "totalResults")]
    pub total_results: usize,
    #[serde(rename = "startIndex")]
    pub start_index: usize,
    #[serde(rename = "itemsPerPage")]
    pub items_per_page: usize,
    #[serde(rename = "Resources")]
    pub resources: Vec<T>,
}

impl<T> ListResponse<T> {
    pub fn new(resources: Vec<T>, total: usize, start_index: usize) -> Self {
        Self {
            schemas: [LIST_SCHEMA],
            total_results: total,
            items_per_page: resources.len(),
            start_index,
            resources,
        }
    }
}

#[derive(Serialize)]
pub struct Meta {
    #[serde(rename = "resourceType")]
    pub resource_type: &'static str,
}

#[derive(Serialize)]
pub struct UserResource {
    pub schemas: [&'static str; 1],
    pub id: String,
    #[serde(rename = "userName")]
    pub user_name: String,
    #[serde(skip_serializing_if = "Option::is_none", rename = "externalId")]
    pub external_id: Option<String>,
    pub name: NameResource,
    #[serde(rename = "displayName")]
    pub display_name: String,
    pub emails: Vec<EmailResource>,
    pub active: bool,
    pub meta: Meta,
}

#[derive(Serialize, Default)]
pub struct NameResource {
    #[serde(rename = "formatted")]
    pub formatted: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct EmailResource {
    pub value: String,
    #[serde(default)]
    pub primary: bool,
}

#[derive(Deserialize)]
pub struct UserRequest {
    #[serde(rename = "userName")]
    pub user_name: String,
    #[serde(default, rename = "externalId")]
    pub external_id: Option<String>,
    #[serde(default)]
    pub name: Option<NameRequest>,
    #[serde(default, rename = "displayName")]
    pub display_name: Option<String>,
    #[serde(default)]
    pub emails: Vec<EmailResource>,
    #[serde(default = "default_true")]
    pub active: bool,
}

#[derive(Deserialize, Default)]
pub struct NameRequest {
    #[serde(default)]
    pub formatted: Option<String>,
    #[serde(default, rename = "givenName")]
    pub given_name: Option<String>,
    #[serde(default, rename = "familyName")]
    pub family_name: Option<String>,
}

fn default_true() -> bool {
    true
}

#[derive(Serialize)]
pub struct GroupResource {
    pub schemas: [&'static str; 1],
    pub id: String,
    #[serde(rename = "displayName")]
    pub display_name: String,
    pub members: Vec<GroupMember>,
    pub meta: Meta,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct GroupMember {
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
}

#[derive(Deserialize)]
pub struct GroupRequest {
    #[serde(rename = "displayName")]
    pub display_name: String,
    #[serde(default)]
    pub members: Vec<GroupMember>,
}

#[derive(Deserialize)]
pub struct PatchRequest {
    #[serde(rename = "Operations")]
    pub operations: Vec<PatchOperation>,
}

#[derive(Deserialize)]
pub struct PatchOperation {
    pub op: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub value: Option<Value>,
}
