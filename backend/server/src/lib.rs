mod endpoints;
pub mod requests;
pub mod types;
pub mod executer;
pub mod scheduler;
pub mod input;
pub mod consts;
pub mod sources;
pub mod evaluator;


use axum::routing::get;
use axum::Router;
use clap::Parser;
use sea_orm::{ActiveModelTrait, Database, DatabaseConnection};
use migration::Migrator;
use sea_orm_migration::prelude::*;
use consts::*;
use types::*;
use std::sync::Arc;
use sea_orm::ActiveValue::Set;
use uuid::Uuid;
use chrono::{DateTime, Utc};
use sea_orm::EntityTrait;

#[tokio::main]
pub async fn main() -> std::io::Result<()> {
    let cli = Cli::parse();

    println!("Starting Gradient Server on {}:{}", cli.ip, cli.port);

    let db = connect_db(&cli).await;

    let state = Arc::new(ServerState {
        db,
        cli,
    });

    create_debug_data(Arc::clone(&state)).await;

    tokio::spawn(scheduler::schedule_evaluation_loop(Arc::clone(&state)));
    tokio::spawn(scheduler::schedule_build_loop(Arc::clone(&state)));

    serve_web(Arc::clone(&state)).await?;

    Ok(())
}

async fn connect_db(cli: &Cli) -> DatabaseConnection {
    let db = Database::connect(cli.database_uri.clone())
        .await
        .expect("Failed to connect to database");
    Migrator::up(&db, None).await.unwrap();
    db
}

async fn serve_web(state: Arc<ServerState>) -> std::io::Result<()> {
    let server_url = format!("{}:{}", state.cli.ip.clone(), state.cli.port.clone());

    let app = Router::new()
        .route("/organization", get(endpoints::get_organizations).post(endpoints::post_organizations))
        .route("/organization/:organization", get(endpoints::get_organization).post(endpoints::post_organization))
        .route("/project/:project", get(endpoints::get_project).post(endpoints::post_project))
        .route("/build/:build", get(endpoints::get_build).post(endpoints::post_build))
        .route("/user/:user", get(endpoints::get_user).post(endpoints::post_user))
        .route("/server", get(endpoints::get_servers).post(endpoints::post_servers))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&server_url).await.unwrap();
    axum::serve(listener, app).await
}

async fn delete_all_data(state: Arc<ServerState>) {
    let users = EUser::find().all(&state.db).await.unwrap();
    for u in users {
        let user: AUser = u.into();
        EUser::delete(user).exec(&state.db).await.unwrap();
    }

    let organizations = EOrganization::find().all(&state.db).await.unwrap();
    for o in organizations {
        let organization: AOrganization = o.into();
        EOrganization::delete(organization).exec(&state.db).await.unwrap();
    }

    let projects = EProject::find().all(&state.db).await.unwrap();
    for p in projects {
        let project: AProject = p.into();
        EProject::delete(project).exec(&state.db).await.unwrap();
    }

    let servers = EServer::find().all(&state.db).await.unwrap();
    for s in servers {
        let server: AServer = s.into();
        EServer::delete(server).exec(&state.db).await.unwrap();
    }

    let evaluations = EEvaluation::find().all(&state.db).await.unwrap();
    for e in evaluations {
        let evaluation: AEvaluation = e.into();
        EEvaluation::delete(evaluation).exec(&state.db).await.unwrap();
    }

    let builds = EBuild::find().all(&state.db).await.unwrap();
    for b in builds {
        let build: ABuild = b.into();
        EBuild::delete(build).exec(&state.db).await.unwrap();
    }
}

async fn create_debug_data(state: Arc<ServerState>) {
    delete_all_data(Arc::clone(&state)).await;
    println!("Deleted all Database data");

    let user = AUser {
        id: Set(Uuid::new_v4()),
        name: Set("Test".to_string()),
        email: Set("tes@were.local".to_string()),
        password: Set("password".to_string()),
        password_salt: Set("salt".to_string()),
        created_at: Set(Utc::now().naive_utc()),
    };

    let user = user.insert(&state.db).await.unwrap();
    println!("Created user {}", user.id);

    let organization = AOrganization {
        id: Set(Uuid::new_v4()),
        name: Set("Test Organization".to_string()),
        description: Set("Test Organization Description".to_string()),
        created_by: Set(user.id),
        created_at: Set(Utc::now().naive_utc()),
    };

    let organization = organization.insert(&state.db).await.unwrap();
    println!("Created organization {}", organization.id);

    let project = AProject {
        id: Set(Uuid::new_v4()),
        organization: Set(organization.id),
        name: Set("Good Project".to_string()),
        description: Set("Test Good Project Description".to_string()),
        repository: Set("git+ssh://gitea@git.wavelens.io:12/Wavelens/GPUTerraform.git?ref=main".to_string()),
        last_evaluation: Set(None),
        last_check_at: Set(*NULL_TIME),
        created_by: Set(user.id),
        created_at: Set(Utc::now().naive_utc()),
    };

    let project = project.insert(&state.db).await.unwrap();
    println!("Created project {}", project.id);

    let server = AServer {
        id: Set(Uuid::new_v4()),
        name: Set("Test Server".to_string()),
        organization: Set(organization.id),
        host: Set("localhost".to_string()),
        port: Set(22),
        architectures: Set(vec![entity::server::Architecture::X86_64Linux]),
        features: Set(vec!["big_parallel".to_string()]),
        last_connection_at: Set(DateTime::from_timestamp(0, 0).unwrap().naive_utc()),
        created_by: Set(user.id),
        created_at: Set(Utc::now().naive_utc()),
    };

    let server = server.insert(&state.db).await.unwrap();
    println!("Created server {}", server.id);

}

