/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use entity::build::BuildStatus;
use entity::evaluation::EvaluationStatus;
use migration::Migrator;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectOptions, Database, DatabaseConnection,
    DbErr, EntityTrait, QueryFilter, QuerySelect,
};
use sea_orm_migration::prelude::*;
use std::sync::Arc;
use std::time::Duration;
use tracing::log::LevelFilter;
use uuid::Uuid;

use super::consts::{BASE_ROLE_ADMIN_ID, BASE_ROLE_VIEW_ID, BASE_ROLE_WRITE_ID};
use super::types::*;

pub async fn connect_db(cli: &Cli) -> Result<DatabaseConnection> {
    let db_url = if let Some(file) = &cli.database_url_file {
        std::fs::read_to_string(file).context("Failed to read database url from file")?
    } else if let Some(url) = &cli.database_url {
        url.clone()
    } else {
        anyhow::bail!("No database url provided")
    };

    // Configure database connection options
    let mut opt = ConnectOptions::new(db_url);

    // Only enable SQL logging at debug level
    if cli.log_level == "debug" {
        opt.sqlx_logging(true)
            .sqlx_logging_level(LevelFilter::Debug);
    } else {
        opt.sqlx_logging(false);
    }

    // Set other connection options
    opt.max_connections(100)
        .min_connections(5)
        .connect_timeout(Duration::from_secs(8))
        .acquire_timeout(Duration::from_secs(8))
        .idle_timeout(Duration::from_secs(8))
        .max_lifetime(Duration::from_secs(8));

    let db = Database::connect(opt)
        .await
        .context("Failed to connect to database")?;
    Migrator::up(&db, None)
        .await
        .context("Failed to run database migrations")?;
    update_db(&db).await.context("Failed to update database")?;
    Ok(db)
}

async fn update_db(db: &DatabaseConnection) -> Result<(), DbErr> {
    let builds = EBuild::find()
        .filter(
            Condition::any()
                .add(CBuild::Status.eq(BuildStatus::Created))
                .add(CBuild::Status.eq(BuildStatus::Queued))
                .add(CBuild::Status.eq(BuildStatus::Building)),
        )
        .all(db)
        .await?;

    for build in builds {
        let mut abuild: ABuild = build.into();
        abuild.status = Set(BuildStatus::Aborted);
        abuild.update(db).await?;
    }

    let evaluations = EEvaluation::find()
        .filter(
            Condition::any()
                .add(CEvaluation::Status.eq(EvaluationStatus::Queued))
                .add(CEvaluation::Status.eq(EvaluationStatus::Evaluating))
                .add(CEvaluation::Status.eq(EvaluationStatus::Building)),
        )
        .all(db)
        .await?;

    for evaluation in evaluations {
        let mut aevaluation: AEvaluation = evaluation.into();
        aevaluation.status = Set(EvaluationStatus::Aborted);
        aevaluation.update(db).await?;
    }

    let base_role_admin = ERole::find_by_id(BASE_ROLE_ADMIN_ID).one(db).await?;

    if base_role_admin.is_none() {
        let arole = ARole {
            id: Set(BASE_ROLE_ADMIN_ID),
            name: Set("Admin".to_string()),
            organization: Set(None),
            permission: Set(0x7FFFFFFFFFFFFFFF),
        };

        arole.insert(db).await?;
    }

    let base_role_write = ERole::find_by_id(BASE_ROLE_WRITE_ID).one(db).await?;

    if base_role_write.is_none() {
        let arole = ARole {
            id: Set(BASE_ROLE_WRITE_ID),
            name: Set("Write".to_string()),
            organization: Set(None),
            permission: Set(0x000000000000000),
        };

        arole.insert(db).await?;
    }

    let base_role_view = ERole::find_by_id(BASE_ROLE_VIEW_ID).one(db).await?;

    if base_role_view.is_none() {
        let arole = ARole {
            id: Set(BASE_ROLE_VIEW_ID),
            name: Set("View".to_string()),
            organization: Set(None),
            permission: Set(0x000000000000000),
        };

        arole.insert(db).await?;
    }

    Ok(())
}

pub async fn add_features(
    state: Arc<ServerState>,
    features: Vec<String>,
    build_id: Option<Uuid>,
    server_id: Option<Uuid>,
) -> Result<()> {
    for f in features {
        let feature = EFeature::find()
            .filter(CFeature::Name.eq(f.clone()))
            .one(&state.db)
            .await
            .context("Failed to query feature")?;

        let feature = if let Some(f) = feature {
            f
        } else {
            let afeature = AFeature {
                id: Set(Uuid::new_v4()),
                name: Set(f),
            };

            afeature
                .insert(&state.db)
                .await
                .context("Failed to insert feature")?
        };

        if let Some(b_id) = build_id {
            let abuild_feature = ABuildFeature {
                id: Set(Uuid::new_v4()),
                build: Set(b_id),
                feature: Set(feature.id),
            };

            abuild_feature
                .insert(&state.db)
                .await
                .context("Failed to insert build feature")?;
        }

        if let Some(s_id) = server_id {
            let aserver_feature = AServerFeature {
                id: Set(Uuid::new_v4()),
                server: Set(s_id),
                feature: Set(feature.id),
            };

            aserver_feature
                .insert(&state.db)
                .await
                .context("Failed to insert server feature")?;
        }
    }
    Ok(())
}

pub async fn get_organization_by_name(
    state: Arc<ServerState>,
    user_id: Uuid,
    name: String,
) -> Result<Option<MOrganization>> {
    Ok(EOrganization::find()
        .join_rev(
            JoinType::InnerJoin,
            EOrganizationUser::belongs_to(entity::organization::Entity)
                .from(COrganizationUser::Organization)
                .to(COrganization::Id)
                .into(),
        )
        .filter(
            Condition::all()
                .add(COrganizationUser::User.eq(user_id))
                .add(COrganization::Name.eq(name)),
        )
        .one(&state.db)
        .await
        .context("Failed to query organization")?)
}

pub async fn get_project_by_name(
    state: Arc<ServerState>,
    user_id: Uuid,
    organization_name: String,
    project_name: String,
) -> Result<Option<(MOrganization, MProject)>> {
    match get_organization_by_name(state.clone(), user_id, organization_name).await? {
        Some(o) => Ok(EProject::find()
            .filter(CProject::Organization.eq(o.id))
            .filter(CProject::Name.eq(project_name))
            .one(&state.db)
            .await
            .context("Failed to query project")?
            .map(|p| (o, p))),
        None => Ok(None),
    }
}

pub async fn get_server_by_name(
    state: Arc<ServerState>,
    user_id: Uuid,
    organization_name: String,
    server_name: String,
) -> Result<Option<(MOrganization, MServer)>> {
    match get_organization_by_name(state.clone(), user_id, organization_name).await? {
        Some(o) => Ok(EServer::find()
            .filter(CServer::Organization.eq(o.id))
            .filter(CServer::Name.eq(server_name))
            .one(&state.db)
            .await
            .context("Failed to query server")?
            .map(|s| (o, s))),
        None => Ok(None),
    }
}

pub async fn get_cache_by_name(
    state: Arc<ServerState>,
    user_id: Uuid,
    name: String,
) -> Result<Option<MCache>> {
    Ok(ECache::find()
        .filter(
            Condition::all()
                .add(CCache::CreatedBy.eq(user_id))
                .add(CCache::Name.eq(name)),
        )
        .one(&state.db)
        .await
        .context("Failed to query cache")?)
}
