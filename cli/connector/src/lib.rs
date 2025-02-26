/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod auth;
pub mod builds;
pub mod evals;
pub mod orgs;
pub mod projects;
pub mod servers;
pub mod user;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct RequestConfig {
    pub server_url: String,
    pub token: Option<String>,
}

#[derive(Debug, PartialEq)]
enum RequestType {
    Get,
    Post,
    Delete,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct BaseResponse<T> {
    pub error: bool,
    pub message: T,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListItem {
    pub id: String,
    pub name: String,
}

pub type ListResponse = Vec<ListItem>;

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

fn get_client(
    config: RequestConfig,
    endpoint: String,
    request_type: RequestType,
    login: bool,
) -> Result<reqwest::RequestBuilder, String> {
    let client = reqwest::Client::new();
    let mut client = if request_type == RequestType::Post {
        client.post(format!("{}/api/v1/{}", config.server_url, endpoint))
    } else if request_type == RequestType::Delete {
        client.delete(format!("{}/api/v1/{}", config.server_url, endpoint))
    } else {
        client.get(format!("{}/api/v1/{}", config.server_url, endpoint))
    };

    client = client.header("Content-Type", "application/json");

    if !login {
        return Ok(client);
    }

    let token = if let Some(token) = config.token {
        token
    } else {
        return Err("Token not set. Use `gradient login` to set it.".to_string());
    };

    client = client.header("Authorization", format!("Bearer {}", token));

    Ok(client)
}

pub async fn health(config: RequestConfig) -> Result<BaseResponse<String>, String> {
    let res = get_client(config, "health".to_string(), RequestType::Get, false)
        .unwrap()
        .send()
        .await;

    let res = match res {
        Ok(res) => res,
        Err(e) => return Err(e.to_string()),
    };

    Ok(parse_response(res).await.unwrap())
}
