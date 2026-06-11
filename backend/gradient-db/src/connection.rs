/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_migration::Migrator;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectOptions, ConnectionTrait, Database,
    DatabaseBackend, DatabaseConnection, DbErr, EntityTrait, IntoActiveModel, QueryFilter,
    QuerySelect, Statement, Value,
};
use sea_orm_migration::prelude::*;
use std::time::Duration;
use tracing::log::LevelFilter;

use super::DbContext;
use crate::permissions::{
    admin_mask, cache_admin_mask, cache_view_mask, cache_write_mask, view_mask, write_mask,
};
use gradient_types::consts::{
    BASE_CACHE_ROLE_ADMIN_ID, BASE_CACHE_ROLE_VIEW_ID, BASE_CACHE_ROLE_WRITE_ID,
    BASE_ROLE_ADMIN_ID, BASE_ROLE_VIEW_ID, BASE_ROLE_WRITE_ID,
};
use gradient_types::*;

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
    let db = Database::connect(make_connect_options(
        cli,
        cli.database.database_max_connections,
        cli.database.database_min_connections,
    )?)
    .await
    .context("Failed to connect to database")?;
    Migrator::install(&db)
        .await
        .context("Failed to install seaql_migrations table")?;
    prune_removed_migrations(&db)
        .await
        .context("Failed to prune removed-migration entries from seaql_migrations")?;
    Migrator::up(&db, None)
        .await
        .context("Failed to run database migrations")?;
    update_db(&db).await.context("Failed to update database")?;
    Ok(db)
}

/// Purge `seaql_migrations` rows whose `version` is no longer in the
/// registered migration list. Without this, sea-orm's validator aborts with
/// "Applied migrations not found in migration list" on installs that ran
/// migrations later removed from the codebase. The set of registered names is
/// derived from `Migrator::migrations()` so it cannot drift from reality.
async fn prune_removed_migrations(db: &DatabaseConnection) -> Result<()> {
    let known: Vec<Value> = Migrator::migrations()
        .iter()
        .map(|m| Value::from(m.name().to_string()))
        .collect();
    if known.is_empty() {
        return Ok(());
    }
    let placeholders: Vec<String> = (1..=known.len()).map(|i| format!("${i}")).collect();
    let sql = format!(
        "DELETE FROM seaql_migrations WHERE version NOT IN ({}) RETURNING version",
        placeholders.join(", ")
    );
    let rows = db
        .query_all(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql,
            known,
        ))
        .await?;
    if !rows.is_empty() {
        let pruned: Vec<String> = rows
            .iter()
            .filter_map(|r| r.try_get::<String>("", "version").ok())
            .collect();
        tracing::info!(?pruned, "pruned orphan seaql_migrations rows");
    }
    Ok(())
}

/// Open a dedicated connection pool for the web/HTTP layer so that axum
/// handlers do not contend with the busy proto/scheduler pool during heavy
/// NarPush traffic.
pub async fn connect_web_db(cli: &Cli) -> Result<DatabaseConnection> {
    Database::connect(make_connect_options(
        cli,
        cli.database.database_web_max_connections,
        cli.database.database_web_min_connections,
    )?)
    .await
    .context("Failed to connect web database pool")
}

/// Evaluations the scheduler re-dispatches on its own after a restart, so
/// startup recovery must leave them alone: `Queued` is re-offered by the eval
/// dispatcher, `Waiting` (evaluated, builds queued for a free worker) is
/// re-driven by build reconcile. Every other active status was running on a
/// now-disconnected worker and is genuinely lost.
fn eval_survives_restart(status: EvaluationStatus) -> bool {
    matches!(status, EvaluationStatus::Queued | EvaluationStatus::Waiting)
}

/// A build survives a restart only if it had not started running
/// (`Created`/`Queued`) *and* its evaluation survives too. A `Building` build
/// was on a lost worker; a queued build under an aborted evaluation goes with
/// it.
fn build_survives_restart(status: BuildStatus, eval_survives: bool) -> bool {
    eval_survives && matches!(status, BuildStatus::Created | BuildStatus::Queued)
}

async fn update_db(db: &DatabaseConnection) -> Result<(), DbErr> {
    // Recover work interrupted by the restart. Only abort what was genuinely
    // lost (mid-fetch/eval/build evaluations, mid-compile builds); leave
    // queued/waiting evaluations and their not-yet-dispatched builds so the
    // scheduler re-offers them on its next tick.
    let surviving_eval_ids: Vec<EvaluationId> = EEvaluation::find()
        .filter(CEvaluation::Status.is_in(EvaluationStatus::ACTIVE))
        .all(db)
        .await?
        .into_iter()
        .filter(|e| eval_survives_restart(e.status))
        .map(|e| e.id)
        .collect();

    let builds = EBuild::find()
        .filter(CBuild::Status.is_in([
            BuildStatus::Created,
            BuildStatus::Queued,
            BuildStatus::Building,
        ]))
        .all(db)
        .await?;

    for build in builds {
        if build_survives_restart(build.status, surviving_eval_ids.contains(&build.evaluation)) {
            continue;
        }
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
        if eval_survives_restart(evaluation.status) {
            continue;
        }

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

    seed_builtin_cache_role(db, BASE_CACHE_ROLE_ADMIN_ID, "Admin", cache_admin_mask()).await?;
    seed_builtin_cache_role(db, BASE_CACHE_ROLE_WRITE_ID, "Write", cache_write_mask()).await?;
    seed_builtin_cache_role(db, BASE_CACHE_ROLE_VIEW_ID, "View", cache_view_mask()).await?;

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
            MRole {
                id: role_id,
                name: name.to_string(),
                permission,
                ..Default::default()
            }
            .into_active_model()
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

async fn seed_builtin_cache_role(
    db: &DatabaseConnection,
    role_id: RoleId,
    name: &str,
    permission: i64,
) -> Result<(), DbErr> {
    match ECacheRole::find_by_id(role_id).one(db).await? {
        None => {
            MCacheRole {
                id: role_id,
                name: name.to_string(),
                permission,
                ..Default::default()
            }
            .into_active_model()
            .insert(db)
            .await?;
        }
        Some(existing) if existing.permission != permission || existing.name != name => {
            let mut active: ACacheRole = existing.into();
            active.name = Set(name.to_string());
            active.permission = Set(permission);
            active.update(db).await?;
        }
        Some(_) => {}
    }
    Ok(())
}

pub async fn add_features(
    ctx: &DbContext,
    features: Vec<String>,
    kind: gradient_entity::feature::FeatureKind,
    derivation_id: Option<DerivationId>,
) -> Result<()> {
    for f in features {
        let feature = EFeature::find()
            .filter(CFeature::Name.eq(f.clone()))
            .filter(CFeature::Kind.eq(kind.clone()))
            .one(&ctx.worker_db)
            .await
            .context("Failed to query feature")?;

        let feature = if let Some(f) = feature {
            f
        } else {
            let afeature = MFeature {
                id: FeatureId::now_v7(),
                name: f,
                kind: kind.clone(),
            }
            .into_active_model();

            afeature
                .insert(&ctx.worker_db)
                .await
                .context("Failed to insert feature")?
        };

        if let Some(d_id) = derivation_id {
            let aderivation_feature = MDerivationFeature {
                id: DerivationFeatureId::now_v7(),
                derivation: d_id,
                feature: feature.id,
            }
            .into_active_model();

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
                .exec(&ctx.worker_db)
                .await
                .context("Failed to insert derivation feature")?;
        }
    }
    Ok(())
}

pub async fn get_organization_by_name(
    ctx: &DbContext,
    user_id: UserId,
    name: String,
) -> Result<Option<MOrganization>> {
    EOrganization::find()
        .join_rev(
            JoinType::InnerJoin,
            EOrganizationUser::belongs_to(gradient_entity::organization::Entity)
                .from(COrganizationUser::Organization)
                .to(COrganization::Id)
                .into(),
        )
        .filter(
            Condition::all()
                .add(COrganizationUser::User.eq(user_id))
                .add(COrganization::Name.eq(name)),
        )
        .one(&ctx.web_db)
        .await
        .context("Failed to query organization")
}

pub async fn get_any_organization_by_name(
    ctx: &DbContext,
    name: String,
) -> Result<Option<MOrganization>> {
    EOrganization::find()
        .filter(COrganization::Name.eq(name))
        .one(&ctx.web_db)
        .await
        .context("Failed to query organization")
}

pub async fn get_project_by_name(
    ctx: &DbContext,
    user_id: UserId,
    organization_name: String,
    project_name: String,
) -> Result<Option<(MOrganization, MProject)>> {
    match get_organization_by_name(ctx, user_id, organization_name).await? {
        Some(o) => Ok(EProject::find()
            .filter(CProject::Organization.eq(o.id))
            .filter(CProject::Name.eq(project_name))
            .one(&ctx.web_db)
            .await
            .context("Failed to query project")?
            .map(|p| (o, p))),
        None => Ok(None),
    }
}

pub async fn get_any_project_by_name(
    ctx: &DbContext,
    organization_name: String,
    project_name: String,
) -> Result<Option<(MOrganization, MProject)>> {
    match get_any_organization_by_name(ctx, organization_name).await? {
        Some(o) => Ok(EProject::find()
            .filter(CProject::Organization.eq(o.id))
            .filter(CProject::Name.eq(project_name))
            .one(&ctx.web_db)
            .await
            .context("Failed to query project")?
            .map(|p| (o, p))),
        None => Ok(None),
    }
}

pub async fn get_cache_by_name(
    ctx: &DbContext,
    user_id: UserId,
    name: String,
) -> Result<Option<MCache>> {
    ECache::find()
        .filter(
            Condition::all()
                .add(CCache::CreatedBy.eq(user_id))
                .add(CCache::Name.eq(name)),
        )
        .one(&ctx.web_db)
        .await
        .context("Failed to query cache")
}

pub async fn get_any_cache_by_name(ctx: &DbContext, name: String) -> Result<Option<MCache>> {
    ECache::find()
        .filter(CCache::Name.eq(name))
        .one(&ctx.web_db)
        .await
        .context("Failed to query cache")
}

#[cfg(test)]
mod startup_recovery_tests {
    use super::{build_survives_restart, eval_survives_restart};
    use gradient_entity::build::BuildStatus;
    use gradient_entity::evaluation::EvaluationStatus;

    #[test]
    fn queued_and_waiting_evaluations_survive_restart() {
        assert!(eval_survives_restart(EvaluationStatus::Queued));
        assert!(eval_survives_restart(EvaluationStatus::Waiting));
    }

    #[test]
    fn actively_running_evaluations_are_aborted_on_restart() {
        for status in [
            EvaluationStatus::Fetching,
            EvaluationStatus::EvaluatingFlake,
            EvaluationStatus::EvaluatingDerivation,
            EvaluationStatus::Building,
        ] {
            assert!(
                !eval_survives_restart(status),
                "{status:?} must not survive"
            );
        }
    }

    #[test]
    fn queued_builds_of_a_surviving_evaluation_survive_restart() {
        // The "eval waiting case": builds queued for a free worker must not be
        // aborted just because the server restarted.
        assert!(build_survives_restart(BuildStatus::Queued, true));
        assert!(build_survives_restart(BuildStatus::Created, true));
    }

    #[test]
    fn running_builds_are_aborted_even_under_a_surviving_evaluation() {
        // A `Building` build was on a now-disconnected worker - genuinely lost.
        assert!(!build_survives_restart(BuildStatus::Building, true));
    }

    #[test]
    fn builds_of_an_aborted_evaluation_are_aborted() {
        assert!(!build_survives_restart(BuildStatus::Queued, false));
        assert!(!build_survives_restart(BuildStatus::Created, false));
    }
}
