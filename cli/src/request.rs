/*
 * SPDX-FileCopyrightText: 2024 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::ConfigKey;

#[derive(Serialize, Deserialize, Debug)]
pub struct BaseResponse<T> {
    pub error: bool,
    pub message: T,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct UserInfoResponse {
    pub id: String,
    pub username: String,
    pub name: String,
    pub email: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct MakeOrganizationRequest {
    pub name: String,
    pub description: String,
    pub use_nix_store: bool,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct MakeProjectRequest {
    pub name: String,
    pub description: String,
    pub repository: String,
    pub evaluation_wildcard: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct MakeServerRequest {
    pub organization_id: String,
    pub name: String,
    pub host: String,
    pub port: i32,
    pub username: String,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListItem {
    pub id: String,
    pub name: String,
}

pub type ListResponse = Vec<ListItem>;

fn get_client(
    config: HashMap<ConfigKey, Option<String>>,
    endpoint: String,
    post: bool,
    login: bool,
) -> Result<reqwest::RequestBuilder, String> {
    let server_url = if let Some(server_url) = config.get(&ConfigKey::Server).unwrap().clone() {
        server_url
    } else {
        return Err(
            "Server URL not set. Use `gradient config server <url>` to set it.".to_string(),
        );
    };

    let client = reqwest::Client::new();
    let mut client = if post {
        client.post(format!("{}/api/{}", server_url, endpoint))
    } else {
        client.get(format!("{}/api/{}", server_url, endpoint))
    };

    client = client.header("Content-Type", "application/json");

    if !login {
        return Ok(client);
    }

    let token = if let Some(token) = config.get(&ConfigKey::AuthToken).unwrap().clone() {
        token
    } else {
        return Err("Token not set. Use `gradient login` to set it.".to_string());
    };

    client = client.header("Authorization", format!("Bearer {}", token));

    Ok(client)
}

async fn parse_response<T: for<'de> Deserialize<'de>>(
    res: reqwest::Response,
) -> Result<BaseResponse<T>, String> {
    let parsed_res = match res.json().await {
        Ok(res) => res,
        Err(e) => {
            eprintln!("{}", e);
            std::process::exit(1);
        }
    };

    Ok(parsed_res)
}

pub async fn health(
    config: HashMap<ConfigKey, Option<String>>,
) -> Result<BaseResponse<String>, String> {
    let res = get_client(config, "health".to_string(), false, false)
        .unwrap()
        .send()
        .await;

    let res = match res {
        Ok(res) => res,
        Err(e) => return Err(e.to_string()),
    };

    Ok(parse_response(res).await.unwrap())
}

pub async fn register(
    config: HashMap<ConfigKey, Option<String>>,
    username: String,
    name: String,
    email: String,
    password: String,
) -> Result<BaseResponse<String>, String> {
    let req = MakeUserRequest {
        username,
        name,
        email,
        password,
    };

    let res = get_client(config, "user/register".to_string(), true, false)
        .unwrap()
        .json(&req)
        .send()
        .await;

    let res = match res {
        Ok(res) => res,
        Err(e) => return Err(e.to_string()),
    };

    Ok(parse_response(res).await.unwrap())
}

pub async fn login(
    config: HashMap<ConfigKey, Option<String>>,
    loginname: String,
    password: String,
) -> Result<BaseResponse<String>, String> {
    let req = MakeLoginRequest {
        loginname,
        password,
    };

    let res = get_client(config, "user/login".to_string(), true, false)
        .unwrap()
        .json(&req)
        .send()
        .await;

    let res = match res {
        Ok(res) => res,
        Err(e) => return Err(e.to_string()),
    };

    Ok(parse_response(res).await.unwrap())
}

pub async fn user_info(
    config: HashMap<ConfigKey, Option<String>>,
) -> Result<BaseResponse<UserInfoResponse>, String> {
    let res = get_client(config, "user/info".to_string(), false, true)
        .unwrap()
        .send()
        .await;

    let res = match res {
        Ok(res) => res,
        Err(e) => return Err(e.to_string()),
    };

    Ok(parse_response(res).await.unwrap())
}

pub async fn show_organization(
    config: HashMap<ConfigKey, Option<String>>,
    organization_id: String,
) -> Result<BaseResponse<String>, String> {
    let res = get_client(
        config,
        format!("organization/{}", organization_id),
        false,
        true,
    )
    .unwrap()
    .send()
    .await;

    let res = match res {
        Ok(res) => res,
        Err(e) => return Err(e.to_string()),
    };

    Ok(parse_response(res).await.unwrap())
}

pub async fn list_organization(
    config: HashMap<ConfigKey, Option<String>>,
) -> Result<BaseResponse<ListResponse>, String> {
    let res = get_client(config, "organization".to_string(), false, true)
        .unwrap()
        .send()
        .await;

    let res = match res {
        Ok(res) => res,
        Err(e) => return Err(e.to_string()),
    };

    Ok(parse_response(res).await.unwrap())
}

pub async fn create_organization(
    config: HashMap<ConfigKey, Option<String>>,
    name: String,
    description: String,
    use_nix_store: bool,
) -> Result<BaseResponse<String>, String> {
    let req = MakeOrganizationRequest {
        name,
        description,
        use_nix_store,
    };

    let res = get_client(config, "organization".to_string(), true, true)
        .unwrap()
        .json(&req)
        .send()
        .await;

    let res = match res {
        Ok(res) => res,
        Err(e) => return Err(e.to_string()),
    };

    Ok(parse_response(res).await.unwrap())
}

pub async fn delete_organization(
    config: HashMap<ConfigKey, Option<String>>,
    organization_id: String,
) -> Result<BaseResponse<String>, String> {
    let res = get_client(
        config,
        format!("organization/{}", organization_id),
        true,
        true,
    )
    .unwrap()
    .send()
    .await;

    let res = match res {
        Ok(res) => res,
        Err(e) => return Err(e.to_string()),
    };

    Ok(parse_response(res).await.unwrap())
}

pub async fn renew_organization_ssh(
    config: HashMap<ConfigKey, Option<String>>,
    organization_id: String,
) -> Result<BaseResponse<String>, String> {
    let res = get_client(
        config,
        format!("organization/{}/ssh", organization_id),
        true,
        true,
    )
    .unwrap()
    .send()
    .await;

    let res = match res {
        Ok(res) => res,
        Err(e) => return Err(e.to_string()),
    };

    Ok(parse_response(res).await.unwrap())
}

pub async fn get_organization_ssh(
    config: HashMap<ConfigKey, Option<String>>,
    organization_id: String,
) -> Result<BaseResponse<String>, String> {
    let res = get_client(
        config,
        format!("organization/{}/ssh", organization_id),
        false,
        true,
    )
    .unwrap()
    .send()
    .await;

    let res = match res {
        Ok(res) => res,
        Err(e) => return Err(e.to_string()),
    };

    Ok(parse_response(res).await.unwrap())
}

pub async fn show_project(
    config: HashMap<ConfigKey, Option<String>>,
    projekt_id: String,
) -> Result<BaseResponse<Vec<String>>, String> {
    let res = get_client(config, format!("project/{}", projekt_id), false, true)
        .unwrap()
        .send()
        .await;

    let res = match res {
        Ok(res) => res,
        Err(e) => return Err(e.to_string()),
    };

    Ok(parse_response(res).await.unwrap())
}

// pub async fn list_project(config: HashMap<ConfigKey, Option<String>>, organization_id: String) -> Result<BaseResponse<ListResponse>, String> {
//     let res = get_client(config, format!("project/{}", organization_id), false, true)
//         .unwrap()
//         .send()
//         .await;

//     let res = match res {
//         Ok(res) => res,
//         Err(e) => return Err(e.to_string()),
//     };

//     Ok(parse_response(res).await.unwrap())
// }

pub async fn create_project(
    config: HashMap<ConfigKey, Option<String>>,
    organization_id: String,
    name: String,
    description: String,
    repository: String,
    evaluation_wildcard: String,
) -> Result<BaseResponse<String>, String> {
    let req = MakeProjectRequest {
        name,
        description,
        repository,
        evaluation_wildcard,
    };

    let res = get_client(
        config,
        format!("organization/{}", organization_id),
        true,
        true,
    )
    .unwrap()
    .json(&req)
    .send()
    .await;

    let res = match res {
        Ok(res) => res,
        Err(e) => return Err(e.to_string()),
    };

    Ok(parse_response(res).await.unwrap())
}

pub async fn delete_project(
    config: HashMap<ConfigKey, Option<String>>,
    project_id: String,
) -> Result<BaseResponse<String>, String> {
    let res = get_client(config, format!("project/{}", project_id), true, true)
        .unwrap()
        .send()
        .await;

    let res = match res {
        Ok(res) => res,
        Err(e) => return Err(e.to_string()),
    };

    Ok(parse_response(res).await.unwrap())
}

pub async fn show_server(
    config: HashMap<ConfigKey, Option<String>>,
    server_id: String,
) -> Result<BaseResponse<Vec<String>>, String> {
    let res = get_client(config, format!("server/{}", server_id), false, true)
        .unwrap()
        .send()
        .await;

    let res = match res {
        Ok(res) => res,
        Err(e) => return Err(e.to_string()),
    };

    Ok(parse_response(res).await.unwrap())
}

pub async fn list_server(
    config: HashMap<ConfigKey, Option<String>>,
) -> Result<BaseResponse<ListResponse>, String> {
    let res = get_client(config, "server".to_string(), false, true)
        .unwrap()
        .send()
        .await;

    let res = match res {
        Ok(res) => res,
        Err(e) => return Err(e.to_string()),
    };

    Ok(parse_response(res).await.unwrap())
}

pub async fn create_server(
    config: HashMap<ConfigKey, Option<String>>,
    organization_id: String,
    name: String,
    host: String,
    port: i32,
    ssh_user: String,
    architectures: Vec<String>,
    features: Vec<String>,
) -> Result<BaseResponse<String>, String> {
    let req = MakeServerRequest {
        organization_id,
        name,
        host,
        port,
        username: ssh_user,
        architectures,
        features,
    };

    let res = get_client(config, "server".to_string(), true, true)
        .unwrap()
        .json(&req)
        .send()
        .await;

    let res = match res {
        Ok(res) => res,
        Err(e) => return Err(e.to_string()),
    };

    Ok(parse_response(res).await.unwrap())
}
