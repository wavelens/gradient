/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::ids::{CacheId, ProjectId};
use gradient_types::{CProjectCache, EProjectCache, MProjectCache};
use sea_orm::{ColumnTrait, Condition, ConnectionTrait, DbErr, EntityTrait, QueryFilter};

pub async fn project_has_writable_cache<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
) -> Result<bool, sea_orm::DbErr> {
    Ok(!crate::cache_paths::project_write_caches(db, project)
        .await?
        .is_empty())
}

pub async fn subscriptions_of<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
) -> Result<Vec<MProjectCache>, DbErr> {
    EProjectCache::find()
        .filter(CProjectCache::Project.eq(project))
        .all(db)
        .await
}

pub async fn subscription<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    cache: CacheId,
) -> Result<Option<MProjectCache>, DbErr> {
    EProjectCache::find()
        .filter(
            Condition::all()
                .add(CProjectCache::Project.eq(project))
                .add(CProjectCache::Cache.eq(cache)),
        )
        .one(db)
        .await
}

pub async fn subscriptions_to<C: ConnectionTrait>(
    db: &C,
    cache: CacheId,
) -> Result<Vec<MProjectCache>, DbErr> {
    EProjectCache::find()
        .filter(CProjectCache::Cache.eq(cache))
        .all(db)
        .await
}

pub async fn subscription_of_any<C: ConnectionTrait>(
    db: &C,
    cache: CacheId,
    projects: Vec<ProjectId>,
) -> Result<Option<MProjectCache>, DbErr> {
    EProjectCache::find()
        .filter(CProjectCache::Cache.eq(cache))
        .filter(CProjectCache::Project.is_in(projects))
        .one(db)
        .await
}

pub async fn first_subscription_of<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
) -> Result<Option<MProjectCache>, DbErr> {
    EProjectCache::find()
        .filter(CProjectCache::Project.eq(project))
        .one(db)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::database::TestDatabase;
    use gradient_fixtures::rows;
    use gradient_migration::{Migrator, MigratorTrait};
    use gradient_types::ids::{ProjectCacheId, TaskId};
    use gradient_types::{ECache, EProject, ETask, EUser, MTask};
    use sea_orm_migration::SchemaManager;

    #[tokio::test]
    async fn migration_sets_project_sign_outputs_true_if_any_task_signed_and_false_if_all_opted_out()
     {
        let db = TestDatabase::create().await;
        let conn = db.connection();
        crate::rows::insert::<EUser, _>(&conn, rows::user())
            .await
            .expect("the owner is written");
        crate::rows::insert::<ECache, _>(&conn, rows::cache())
            .await
            .expect("the cache is written");
        let mixed = rows::project();
        let opted_out = rows::project_with_id(ProjectId::now_v7(), "opted-out");
        let without_tasks = rows::project_with_id(ProjectId::now_v7(), "without-tasks");
        for project in [&mixed, &opted_out, &without_tasks] {
            crate::rows::insert::<EProject, _>(&conn, project.clone())
                .await
                .expect("the project is written");
            crate::rows::insert::<EProjectCache, _>(
                &conn,
                MProjectCache {
                    id: ProjectCacheId::now_v7(),
                    project: project.id,
                    cache: rows::cache().id,
                    ..Default::default()
                },
            )
            .await
            .expect("the subscription is written");
        }
        let mut unsigned = Vec::new();
        for (project, name, signs) in [
            (&mixed, "signs", true),
            (&mixed, "skips", false),
            (&opted_out, "skips", false),
        ] {
            let task = crate::rows::insert::<ETask, _>(
                &conn,
                MTask {
                    id: TaskId::now_v7(),
                    project: project.id,
                    name: name.into(),
                    ..rows::task()
                },
            )
            .await
            .expect("the task is written");
            if !signs {
                unsigned.push(format!("'{}'", task.id));
            }
        }
        let migration = Migrator::migrations()
            .into_iter()
            .find(|m| m.name() == "m20261206_000000_add_sign_outputs_to_project_cache")
            .expect("the migration is registered");
        migration
            .down(&SchemaManager::new(&conn))
            .await
            .expect("the migration reverts");
        db.execute(&format!(
            "UPDATE task SET sign_cache = false WHERE id IN ({});",
            unsigned.join(", ")
        ))
        .await;

        migration
            .up(&SchemaManager::new(&conn))
            .await
            .expect("the migration applies");

        let conn = &conn;
        let signs = |project: ProjectId| async move {
            subscription(conn, project, rows::cache().id)
                .await
                .expect("the subscription reads")
                .expect("the subscription exists")
                .sign_outputs
        };
        assert!(signs(mixed.id).await);
        assert!(!signs(opted_out.id).await);
        assert!(signs(without_tasks.id).await);
        db.finish().await;
    }
}
