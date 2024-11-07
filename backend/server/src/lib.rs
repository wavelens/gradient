mod endpoints;
pub mod requests;
pub mod tables;
pub mod types;
pub mod executer;
pub mod scheduler;
pub mod input;

use axum::routing::{get, post};
use axum::Router;
use sea_orm::{ActiveModelTrait, Database};
use migration::Migrator;
use std::env;
use sea_orm_migration::prelude::*;
use types::*;
use std::sync::Arc;
use sea_orm::ActiveValue::Set;
use uuid::Uuid;
use chrono::{DateTime, Utc};

#[tokio::main]
pub async fn main() -> std::io::Result<()> {
    let db = connect_db().await;

    create_debug_data(Arc::clone(&db)).await;

    tokio::spawn(scheduler::schedule_project_loop(Arc::clone(&db)));
    tokio::spawn(scheduler::schedule_build_loop(Arc::clone(&db)));

    serve_web(Arc::clone(&db)).await?;

    Ok(())
}

async fn connect_db() -> DBConn {
    let db_url = env::var("GRADIENT_DATABASE_URL").expect("GRADIENT_DATABASE_URL must be set");
    let db = Database::connect(db_url)
        .await
        .expect("Failed to connect to database");
    Migrator::up(&db, None).await.unwrap();
    Arc::new(db)
}

async fn serve_web(db: DBConn) -> std::io::Result<()> {
    // TODO: use clap
    let server_ip = env::var("GRADIENT_IP").unwrap_or("127.0.0.1".to_string());
    let server_port = env::var("GRADIENT_PORT").unwrap_or("3000".to_string());
    let server_url = format!("{}:{}", server_ip, server_port);

    let state = AppState {
        conn: db,
    };

    let app = Router::new()
        .route("/organization", get(endpoints::get_organizations).post(endpoints::post_organizations))
        .route("/organization/:organization", get(endpoints::get_organization).post(endpoints::post_project))
        .route("/project/:project", get(endpoints::get_project).post(endpoints::post_project))
        .route("/build/:build", get(endpoints::get_build).post(endpoints::post_build))
        .route("/user/:user", get(endpoints::get_user).post(endpoints::post_user))
        .route("/server", get(endpoints::get_servers).post(endpoints::post_servers))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&server_url).await.unwrap();
    axum::serve(listener, app).await
}

async fn create_debug_data(db: DBConn) {
    let user = entity::user::ActiveModel {
        id: Set(Uuid::new_v4()),
        name: Set("Test".to_string()),
        email: Set("tes@were.local".to_string()),
        password: Set("password".to_string()),
        password_salt: Set("salt".to_string()),
        created_at: Set(Utc::now()),
    };

    // let db: DatabaseConnection = db.into();

    let user = user.insert(&*db).await.unwrap();
    println!("Created user {}", user.id);

    let organization = entity::organization::ActiveModel {
        id: Set(Uuid::new_v4()),
        name: Set("Test".to_string()),
        description: Set("Test".to_string()),
        created_by: Set(user.id),
        created_at: Set(Utc::now()),
    };

    let organization = organization.insert(&*db).await.unwrap();
    println!("Created organization {}", organization.id);

    let project = entity::project::ActiveModel {
        id: Set(Uuid::new_v4()),
        organization: Set(organization.id),
        name: Set("Test".to_string()),
        description: Set("Test".to_string()),
        last_check_at: Set(DateTime::from_timestamp(0, 0).unwrap()),
        created_by: Set(user.id),
        created_at: Set(Utc::now()),
    };

    let project = project.insert(&*db).await.unwrap();
    println!("Created project {}", project.id);

    let server = entity::server::ActiveModel {
        id: Set(Uuid::new_v4()),
        organization: Set(organization.id),
        host: Set("localhost".to_string()),
        port: Set(22),
        architectures: Set(vec![entity::server::Architecture::X86_64Linux]),
        features: Set(vec!["big_parallel".to_string()]),
        last_connection_at: Set(DateTime::from_timestamp(0, 0).unwrap()),
        created_by: Set(user.id),
        created_at: Set(Utc::now()),
    };

    let server = server.insert(&*db).await.unwrap();
    println!("Created server {}", server.id);

}

