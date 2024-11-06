use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Serialize, Deserialize, Debug)]
// #[sea_orm(table_name = "organizations")]
pub struct Organization {
    // #[sea_orm(primary_key)]
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub created_by: Uuid,
    pub created_at: i64,
}

#[derive(Serialize, Deserialize, Debug)]
// #[sea_orm(table_name = "projects")]
pub struct Project {
    // #[sea_orm(primary_key)]
    pub id: Uuid,
    pub organization_id: Uuid,
    pub name: String,
    pub description: String,
    pub last_check_at: i64,
    pub created_by: Uuid,
    pub created_at: i64,
}

#[derive(Serialize, Deserialize, Debug)]
// #[sea_orm(table_name = "builds")]
pub struct Build {
    // #[sea_orm(primary_key)]
    pub id: Uuid,
    pub project_id: Uuid,
    pub path: String,
    pub dependencies: Vec<Uuid>,
    pub created_at: i64,
}

#[derive(Serialize, Deserialize, Debug)]
// #[sea_orm(table_name = "users")]
pub struct User {
    // #[sea_orm(primary_key)]
    pub id: Uuid,
    pub username: String,
    pub email: String,
    pub password_salt: String,
    pub password: String,
    pub created_at: i64,
}

#[derive(Serialize, Deserialize, Debug)]
// #[sea_orm(table_name = "servers")]
pub struct Server {
    // #[sea_orm(primary_key)]
    pub id: Uuid,
    pub organization_id: Uuid,
    pub url: String,
    pub connected: bool,
    pub last_connection_at: i64,
    pub created_at: i64,
}
