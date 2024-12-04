/*
 * SPDX-FileCopyrightText: 2024 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only OR WL-1.0
 */

pub mod auth;
pub mod consts;
mod endpoints;
pub mod evaluator;
pub mod executer;
pub mod input;
pub mod requests;
pub mod scheduler;
pub mod sources;
pub mod types;

use axum::routing::{get, post};
use axum::{middleware, Router};
use chrono::{DateTime, Utc};
use clap::Parser;
use consts::*;
use migration::Migrator;
// use oauth2::{basic::BasicClient, AuthUrl, ClientId, ClientSecret, TokenUrl};
use password_auth::generate_hash;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, Condition, Database, DatabaseConnection,
    EntityTrait, QueryFilter,
};
use sea_orm_migration::prelude::*;
use std::sync::Arc;
use types::*;
use uuid::Uuid;

#[tokio::main]
pub async fn main() -> std::io::Result<()> {
    let cli = Cli::parse();

    println!("Starting Gradient Server on {}:{}", cli.ip, cli.port);

    let db = connect_db(&cli).await;

    let state = Arc::new(ServerState { db, cli });

    cleanup_database(Arc::clone(&state)).await;

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

    // TODO: add oauth
    // let oauth_client = if state.cli.oauth_enabled {
    //     Some(BasicClient::new(
    //         ClientId::new(state.cli.oauth_client_id.clone().unwrap()),
    //         Some(ClientSecret::new(
    //             state.cli.oauth_client_secret.clone().unwrap(),
    //         )),
    //         AuthUrl::new(state.cli.oauth_auth_url.clone().unwrap()).unwrap(),
    //         Some(TokenUrl::new(state.cli.oauth_token_url.clone().unwrap()).unwrap()),
    //     ))
    // } else {
    //     None
    // };

    let app = Router::new()
        .route(
            "/api/organization",
            get(endpoints::get_organizations).post(endpoints::post_organizations),
        )
        .route(
            "/api/organization/:organization",
            get(endpoints::get_organization).post(endpoints::post_organization),
        )
        .route(
            "/api/organization/:organization/ssh",
            get(endpoints::get_organization_ssh).post(endpoints::post_organization_ssh),
        )
        .route(
            "/api/project/:project",
            get(endpoints::get_project).post(endpoints::post_project),
        )
        .route(
            "/api/project/:project/check-repository",
            post(endpoints::post_project_check_repository),
        )
        .route(
            "/api/build/:build",
            get(endpoints::get_build).post(endpoints::post_build),
        )
        .route(
            "/api/user/settings/:user",
            get(endpoints::get_user).post(endpoints::post_user),
        )
        .route("/api/user/api", post(endpoints::post_api_key))
        .route(
            "/api/server",
            get(endpoints::get_servers).post(endpoints::post_servers),
        )
        .route(
            "/api/server/:server/check",
            post(endpoints::post_server_check),
        )
        .route_layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            auth::authorize,
        ))
        .route("/api/user/login", post(endpoints::post_login))
        .route("/api/user/logout", post(endpoints::post_logout))
        .route("/api/user/register", post(endpoints::post_register))
        .route("/api/health", get(endpoints::get_health))
        .fallback(endpoints::handle_404)
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&server_url).await.unwrap();
    axum::serve(listener, app).await
}

async fn cleanup_database(state: Arc<ServerState>) {
    create_debug_data(Arc::clone(&state)).await;
    let evaluations = EEvaluation::find()
        .filter(
            Condition::any()
                .add(CEvaluation::Status.eq(entity::evaluation::EvaluationStatus::Queued))
                .add(CEvaluation::Status.eq(entity::evaluation::EvaluationStatus::Evaluating))
                .add(CEvaluation::Status.eq(entity::evaluation::EvaluationStatus::Building)),
        )
        .all(&state.db)
        .await
        .unwrap();

    for e in evaluations {
        println!("Cleaning up evaluation {}", e.id);
        let mut aevaluation: AEvaluation = e.into();
        aevaluation.status = Set(entity::evaluation::EvaluationStatus::Failed);
        aevaluation.update(&state.db).await.unwrap();
    }

    let builds = EBuild::find()
        .filter(
            Condition::any()
                .add(CBuild::Status.eq(entity::build::BuildStatus::Created))
                .add(CBuild::Status.eq(entity::build::BuildStatus::Queued))
                .add(CBuild::Status.eq(entity::build::BuildStatus::Building)),
        )
        .all(&state.db)
        .await
        .unwrap();

    for b in builds {
        println!("Cleaning up build {}", b.id);
        let mut abuild: ABuild = b.into();
        abuild.status = Set(entity::build::BuildStatus::Failed);
        abuild.updated_at = Set(Utc::now().naive_utc());
        abuild.update(&state.db).await.unwrap();
    }
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
        EOrganization::delete(organization)
            .exec(&state.db)
            .await
            .unwrap();
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
        EEvaluation::delete(evaluation)
            .exec(&state.db)
            .await
            .unwrap();
    }

    let builds = EBuild::find().all(&state.db).await.unwrap();
    for b in builds {
        let build: ABuild = b.into();
        EBuild::delete(build).exec(&state.db).await.unwrap();
    }

    let build_dependencies = EBuildDependency::find().all(&state.db).await.unwrap();
    for bd in build_dependencies {
        let build_dependency: ABuildDependency = bd.into();
        EBuildDependency::delete(build_dependency)
            .exec(&state.db)
            .await
            .unwrap();
    }

    let apis = EApi::find().all(&state.db).await.unwrap();
    for a in apis {
        let api: AApi = a.into();
        EApi::delete(api).exec(&state.db).await.unwrap();
    }

    let features = EFeature::find().all(&state.db).await.unwrap();
    for f in features {
        let feature: AFeature = f.into();
        EFeature::delete(feature).exec(&state.db).await.unwrap();
    }

    let server_architectures = EServerArchitecture::find().all(&state.db).await.unwrap();
    for sa in server_architectures {
        let server_architecture: AServerArchitecture = sa.into();
        EServerArchitecture::delete(server_architecture)
            .exec(&state.db)
            .await
            .unwrap();
    }

    let server_features = EServerFeature::find().all(&state.db).await.unwrap();
    for sf in server_features {
        let server_feature: AServerFeature = sf.into();
        EServerFeature::delete(server_feature)
            .exec(&state.db)
            .await
            .unwrap();
    }

    let build_features = EBuildFeature::find().all(&state.db).await.unwrap();
    for bf in build_features {
        let build_feature: ABuildFeature = bf.into();
        EBuildFeature::delete(build_feature)
            .exec(&state.db)
            .await
            .unwrap();
    }

    let commits = ECommit::find().all(&state.db).await.unwrap();
    for c in commits {
        let commit: ACommit = c.into();
        ECommit::delete(commit).exec(&state.db).await.unwrap();
    }
}

async fn create_debug_data(state: Arc<ServerState>) {
    if !state.cli.debug {
        return;
    }

    delete_all_data(Arc::clone(&state)).await;
    println!("Deleted all Database data");

    let user = AUser {
        id: Set(Uuid::new_v4()),
        username: Set("test".to_string()),
        name: Set("Test".to_string()),
        email: Set("tes@were.local".to_string()),
        password: Set(generate_hash("password")),
        last_login_at: Set(*NULL_TIME),
        created_at: Set(Utc::now().naive_utc()),
    };

    let user = user.insert(&state.db).await.unwrap();
    println!("Created user {}", user.id);

    let (private_key, public_key) =
        sources::generate_ssh_key(state.cli.crypt_secret.clone()).unwrap();

    let organization = AOrganization {
        id: Set(Uuid::new_v4()),
        name: Set("Test Organization".to_string()),
        description: Set("Test Organization Description".to_string()),
        public_key: Set(public_key),
        private_key: Set(private_key),
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
        repository: Set("ssh://git@github.com/Wavelens/Gradient.git".to_string()),
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
        username: Set("dennis".to_string()),
        last_connection_at: Set(DateTime::from_timestamp(0, 0).unwrap().naive_utc()),
        created_by: Set(user.id),
        created_at: Set(Utc::now().naive_utc()),
    };

    let server = server.insert(&state.db).await.unwrap();
    println!("Created server {}", server.id);

    let server_architecture = AServerArchitecture {
        id: Set(Uuid::new_v4()),
        server: Set(server.id),
        architecture: Set(entity::server::Architecture::X86_64Linux),
    };

    let server_architecture = server_architecture.insert(&state.db).await.unwrap();
    println!("Created server architecture {}", server_architecture.id);

    let feature = AFeature {
        id: Set(Uuid::new_v4()),
        name: Set("big-parallel".to_string()),
    };

    let feature = feature.insert(&state.db).await.unwrap();
    println!("Created feature {}", feature.id);

    let server_feature = AServerFeature {
        id: Set(Uuid::new_v4()),
        server: Set(server.id),
        feature: Set(feature.id),
    };

    let server_feature = server_feature.insert(&state.db).await.unwrap();
    println!("Created server feature {}", server_feature.id);
}
