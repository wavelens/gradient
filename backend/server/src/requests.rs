use serde::{Deserialize, Serialize};
use entity::server::Architecture;

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
    pub name: String,
    pub host: String,
    pub port: i32,
    pub architectures: Vec<Architecture>,
    pub features: Vec<String>,
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
