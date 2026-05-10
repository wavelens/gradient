/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
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

use crate::permissions::{admin_mask, view_mask, write_mask};
use crate::types::consts::{BASE_ROLE_ADMIN_ID, BASE_ROLE_VIEW_ID, BASE_ROLE_WRITE_ID};
use crate::types::*;

fn db_url(cli: &Cli) -> Result<String> {
    if let Some(file) = &cli.database.database_url_file {
        Ok(std::fs::read_to_string(file).context("Failed to read database url from file")?)
    } else if let Some(url) = &cli.database.database_url {
        Ok(url.clone())
    } else {
        anyhow::bail!("No database url provided")
    }
}

fn make_connect_options(
    cli: &Cli,
    max_connections: u32,
    min_connections: u32,
) -> Result<ConnectOptions> {
    let mut opt = ConnectOptions::new(db_url(cli)?);

    // Only enable SQL logging at trace level
    if cli.logging.log_level == "trace" {
        opt.sqlx_logging(true)
            .sqlx_logging_level(LevelFilter::Trace);
    } else {
        opt.sqlx_logging(false);
    }

    opt.max_connections(max_connections)
        .min_connections(min_connections)
        .connect_timeout(Duration::from_secs(8))
        .acquire_timeout(Duration::from_secs(8))
        .idle_timeout(Duration::from_secs(600))
        .max_lifetime(Duration::from_secs(1800));

    Ok(opt)
}

pub async fn connect_db(cli: &Cli) -> Result<DatabaseConnection> {
    let db = Database::connect(make_connect_options(cli, 100, 5)?)
        .await
        .context("Failed to connect to database")?;
    Migrator::up(&db, None)
        .await
        .context("Failed to run database migrations")?;
    update_db(&db).await.context("Failed to update database")?;
    Ok(db)
}

/// Open a dedicated connection pool for the web/HTTP layer so that axum
/// handlers do not contend with the busy proto/scheduler pool during heavy
/// NarPush traffic.
pub async fn connect_web_db(cli: &Cli) -> Result<DatabaseConnection> {
    Database::connect(make_connect_options(cli, 32, 2)?)
        .await
        .context("Failed to connect web database pool")
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
        abuild.via = Set(None);
        abuild.update(db).await?;
    }

    let evaluations = EEvaluation::find()
        .filter(CEvaluation::Status.is_in(EvaluationStatus::ACTIVE))
        .all(db)
        .await?;

    for evaluation in evaluations {
        // Direct-build evaluations have no project; skip force_evaluation for those.
        if let Some(project_id) = evaluation.project
            && let Some(project) = EProject::find_by_id(project_id).one(db).await?
        {
            let mut aproject: AProject = project.into();
            aproject.force_evaluation = Set(true);
            aproject.update(db).await?;
        }

        let mut aevaluation: AEvaluation = evaluation.into();
        aevaluation.status = Set(EvaluationStatus::Aborted);
        aevaluation.update(db).await?;
    }

    seed_builtin_role(db, BASE_ROLE_ADMIN_ID, "Admin", admin_mask()).await?;
    seed_builtin_role(db, BASE_ROLE_WRITE_ID, "Write", write_mask()).await?;
    seed_builtin_role(db, BASE_ROLE_VIEW_ID, "View", view_mask()).await?;

    Ok(())
}

/// Insert or refresh a built-in role.
///
/// Built-in roles are global (`organization = NULL`) and their canonical
/// permission bitmasks are owned by [`crate::permissions`]. Refreshing on every
/// startup means upgrades that add new capabilities propagate to existing
/// installations without a manual migration; the role name is also kept in
/// sync, which avoids drift if an operator renames a built-in role by hand.
async fn seed_builtin_role(
    db: &DatabaseConnection,
    role_id: RoleId,
    name: &str,
    permission: i64,
) -> Result<(), DbErr> {
    match ERole::find_by_id(role_id).one(db).await? {
        None => {
            ARole {
                id: Set(role_id),
                name: Set(name.to_string()),
                organization: Set(None),
                permission: Set(permission),
                managed: Set(false),
            }
            .insert(db)
            .await?;
        }
        Some(existing) if existing.permission != permission || existing.name != name => {
            let mut active: ARole = existing.into();
            active.name = Set(name.to_string());
            active.permission = Set(permission);
            active.update(db).await?;
        }
        Some(_) => {}
    }
    Ok(())
}

pub async fn add_features(
    state: Arc<ServerState>,
    features: Vec<String>,
    kind: entity::feature::FeatureKind,
    derivation_id: Option<DerivationId>,
) -> Result<()> {
    for f in features {
        let feature = EFeature::find()
            .filter(CFeature::Name.eq(f.clone()))
            .filter(CFeature::Kind.eq(kind.clone()))
            .one(&state.worker_db)
            .await
            .context("Failed to query feature")?;

        let feature = if let Some(f) = feature {
            f
        } else {
            let afeature = AFeature {
                id: Set(FeatureId::now_v7()),
                name: Set(f),
                kind: Set(kind.clone()),
            };

            afeature
                .insert(&state.worker_db)
                .await
                .context("Failed to insert feature")?
        };

        if let Some(d_id) = derivation_id {
            let aderivation_feature = ADerivationFeature {
                id: Set(DerivationFeatureId::now_v7()),
                derivation: Set(d_id),
                feature: Set(feature.id),
            };

            // `derivation_feature` has a UNIQUE (derivation, feature) index;
            // re-discovering an already-known edge during a fresh evaluation
            // would otherwise blow up with a constraint violation and abort
            // the whole eval-result handler. `ON CONFLICT DO NOTHING` makes
            // the insert idempotent.
            EDerivationFeature::insert(aderivation_feature)
                .on_conflict(
                    sea_orm::sea_query::OnConflict::columns([
                        CDerivationFeature::Derivation,
                        CDerivationFeature::Feature,
                    ])
                    .do_nothing()
                    .to_owned(),
                )
                .do_nothing()
                .exec(&state.worker_db)
                .await
                .context("Failed to insert derivation feature")?;
        }
    }
    Ok(())
}

pub async fn get_organization_by_name(
    state: Arc<ServerState>,
    user_id: UserId,
    name: String,
) -> Result<Option<MOrganization>> {
    EOrganization::find()
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
        .one(&state.web_db)
        .await
        .context("Failed to query organization")
}

pub async fn get_any_organization_by_name(
    state: Arc<ServerState>,
    name: String,
) -> Result<Option<MOrganization>> {
    EOrganization::find()
        .filter(COrganization::Name.eq(name))
        .one(&state.web_db)
        .await
        .context("Failed to query organization")
}

pub async fn get_project_by_name(
    state: Arc<ServerState>,
    user_id: UserId,
    organization_name: String,
    project_name: String,
) -> Result<Option<(MOrganization, MProject)>> {
    match get_organization_by_name(state.clone(), user_id, organization_name).await? {
        Some(o) => Ok(EProject::find()
            .filter(CProject::Organization.eq(o.id))
            .filter(CProject::Name.eq(project_name))
            .one(&state.web_db)
            .await
            .context("Failed to query project")?
            .map(|p| (o, p))),
        None => Ok(None),
    }
}

pub async fn get_any_project_by_name(
    state: Arc<ServerState>,
    organization_name: String,
    project_name: String,
) -> Result<Option<(MOrganization, MProject)>> {
    match get_any_organization_by_name(state.clone(), organization_name).await? {
        Some(o) => Ok(EProject::find()
            .filter(CProject::Organization.eq(o.id))
            .filter(CProject::Name.eq(project_name))
            .one(&state.web_db)
            .await
            .context("Failed to query project")?
            .map(|p| (o, p))),
        None => Ok(None),
    }
}

pub async fn get_cache_by_name(
    state: Arc<ServerState>,
    user_id: UserId,
    name: String,
) -> Result<Option<MCache>> {
    ECache::find()
        .filter(
            Condition::all()
                .add(CCache::CreatedBy.eq(user_id))
                .add(CCache::Name.eq(name)),
        )
        .one(&state.web_db)
        .await
        .context("Failed to query cache")
}

pub async fn get_any_cache_by_name(
    state: Arc<ServerState>,
    name: String,
) -> Result<Option<MCache>> {
    ECache::find()
        .filter(CCache::Name.eq(name))
        .one(&state.web_db)
        .await
        .context("Failed to query cache")
}
