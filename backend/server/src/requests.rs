/*
 * SPDX-FileCopyrightText: 2024 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only OR WL-1.0
 */

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
pub struct BaseResponse<T> {
    pub error: bool,
    pub message: T,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct MakeOrganizationRequest {
    pub name: String,
    pub description: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct MakeProjectRequest {
    pub name: String,
    pub description: String,
    pub repository: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct MakeServerRequest {
    pub organization_id: String,
    pub name: String,
    pub host: String,
    pub port: i32,
    pub username: String,
    pub public_key: String,
    pub private_key: String,
    pub architectures: Vec<String>,
    pub features: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct MakeBuildRequest {
    pub log_streaming: bool,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct MakeApiKeyRequest {
    pub name: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct MakeUserRequest {
    pub username: String,
    pub name: String,
    pub email: String,
    pub password: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct MakeLoginRequest {
    pub loginname: String,
    pub password: String,
}
