mod endpoints;
pub mod migration;
pub mod model;
pub mod requests;
pub mod tables;
pub mod types;

use axum::routing::{get, post};
use axum::Router;
use sea_orm::Database;
use std::env;
use migration::migrator::Migrator;
use sea_orm_migration::prelude::*;
use types::*;



#[tokio::main]
async fn main() -> std::io::Result<()> {
    let server_url = env::var("GRADIENT_IP").unwrap_or("127.0.0.1".to_string());
    let db_url = env::var("GRADIENT_DATABASE_URL").expect("GRADIENT_DATABASE_URL must be set");

    let db = Database::connect(db_url)
        .await
        .expect("Failed to connect to database");
    Migrator::up(&db, None).await.unwrap();

    let state = AppState {
        conn: db,
    };

    let app = Router::new()
        .route("/project", get(endpoints::get_projects).post(endpoints::post_projects))
        .route("/project/:project", get(endpoints::get_project).post(endpoints::post_project))
        .route("/jobset/:jobset", get(endpoints::get_jobset).post(endpoints::post_jobset))
        .route("/build/:build", get(endpoints::get_build).post(endpoints::post_build))
        .route("/user/:user", get(endpoints::get_user).post(endpoints::post_user))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&server_url).await.unwrap();
    axum::serve(listener, app).await?;

    Ok(())
}

