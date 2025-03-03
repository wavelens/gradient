/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use migration::Migrator;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, Database, DatabaseConnection, EntityTrait,
    QueryFilter,
};
use sea_orm_migration::prelude::*;
use std::sync::Arc;
use uuid::Uuid;

use super::types::*;

pub async fn connect_db(cli: &Cli) -> DatabaseConnection {
    let db = Database::connect(cli.database_uri.clone())
        .await
        .expect("Failed to connect to database");
    Migrator::up(&db, None).await.unwrap();
    db
}

pub async fn add_features(
    state: Arc<ServerState>,
    features: Vec<String>,
    build_id: Option<Uuid>,
    server_id: Option<Uuid>,
) {
    for f in features {
        let feature = EFeature::find()
            .filter(CFeature::Name.eq(f.clone()))
            .one(&state.db)
            .await
            .unwrap();

        let feature = if let Some(f) = feature {
            f
        } else {
            let afeature = AFeature {
                id: Set(Uuid::new_v4()),
                name: Set(f),
            };

            afeature.insert(&state.db).await.unwrap()
        };

        if let Some(b_id) = build_id {
            let abuild_feature = ABuildFeature {
                id: Set(Uuid::new_v4()),
                build: Set(b_id),
                feature: Set(feature.id),
            };

            abuild_feature.insert(&state.db).await.unwrap();
        }

        if let Some(s_id) = server_id {
            let aserver_feature = AServerFeature {
                id: Set(Uuid::new_v4()),
                server: Set(s_id),
                feature: Set(feature.id),
            };

            aserver_feature.insert(&state.db).await.unwrap();
        }
    }
}

pub async fn get_organization_by_name(
    state: Arc<ServerState>,
    user_id: Uuid,
    name: String,
) -> Option<MOrganization> {
    EOrganization::find()
        .filter(
            Condition::all()
                .add(COrganization::CreatedBy.eq(user_id))
                .add(COrganization::Name.eq(name)),
        )
        .one(&state.db)
        .await
        .unwrap()
}

pub async fn get_project_by_name(
    state: Arc<ServerState>,
    user_id: Uuid,
    organization_name: String,
    project_name: String,
) -> Option<(MOrganization, MProject)> {
    match get_organization_by_name(state.clone(), user_id, organization_name).await {
        Some(o) => EProject::find()
            .filter(CProject::Organization.eq(o.id))
            .filter(CProject::Name.eq(project_name))
            .one(&state.db)
            .await
            .unwrap()
            .map(|p| (o, p)),
        None => None,
    }
}

pub async fn get_server_by_name(
    state: Arc<ServerState>,
    user_id: Uuid,
    organization_name: String,
    server_name: String,
) -> Option<(MOrganization, MServer)> {
    match get_organization_by_name(state.clone(), user_id, organization_name).await {
        Some(o) => EServer::find()
            .filter(CServer::Organization.eq(o.id))
            .filter(CServer::Name.eq(server_name))
            .one(&state.db)
            .await
            .unwrap()
            .map(|s| (o, s)),
        None => None,
    }
}

pub async fn get_cache_by_name(
    state: Arc<ServerState>,
    user_id: Uuid,
    name: String,
) -> Option<MCache> {
    ECache::find()
        .filter(
            Condition::all()
                .add(CCache::CreatedBy.eq(user_id))
                .add(CCache::Name.eq(name)),
        )
        .one(&state.db)
        .await
        .unwrap()
}
