mod endpoints;
pub mod migration;
pub mod model;
pub mod requests;
pub mod tables;
pub mod types;
pub mod executer;
pub mod scheduler;
pub mod input;

use axum::routing::{get, post};
use axum::Router;
use sea_orm::Database;
use std::env;
use migration::migrator::Migrator;
use sea_orm_migration::prelude::*;
use types::*;


#[tokio::main]
async fn main() -> std::io::Result<()> {
    tokio::spawn(scheduler::schedule_project_loop());
    tokio::spawn(scheduler::schedule_build_loop());

    serve_web().await?;

    Ok(())
}

async fn serve_web() -> std::io::Result<()> {
    // TODO: use clap
    let server_ip = env::var("GRADIENT_IP").unwrap_or("127.0.0.1".to_string());
    let server_port = env::var("GRADIENT_PORT").unwrap_or("3000".to_string());
        let server_url = format!("{}:{}", server_ip, server_port);
        let db_url = env::var("GRADIENT_DATABASE_URL").expect("GRADIENT_DATABASE_URL must be set");

    // let db = Database::connect(db_url)
    //     .await
    //     .expect("Failed to connect to database");
    // Migrator::up(&db, None).await.unwrap();

    // let state = AppState {
    //     conn: db,
    // };

    let app = Router::new()
        .route("/organization", get(endpoints::get_organizations).post(endpoints::post_organizations))
        .route("/organization/:organization", get(endpoints::get_organization).post(endpoints::post_project))
        .route("/project/:project", get(endpoints::get_project).post(endpoints::post_project))
        .route("/build/:build", get(endpoints::get_build).post(endpoints::post_build))
        .route("/user/:user", get(endpoints::get_user).post(endpoints::post_user))
        .route("/server", get(endpoints::get_servers).post(endpoints::post_servers));

        // .with_state(state);

    let listener = tokio::net::TcpListener::bind(&server_url).await.unwrap();
    axum::serve(listener, app).await
}

