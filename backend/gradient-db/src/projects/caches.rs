/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::ids::{CacheId, ProjectId};
use gradient_types::{CProjectCache, EProjectCache, MProjectCache};
use sea_orm::{
    ColumnTrait, Condition, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QuerySelect,
};

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

pub async fn project_ids_with_read_access_to_cache<C: ConnectionTrait>(
    db: &C,
    cache: CacheId,
) -> Result<Vec<ProjectId>, DbErr> {
    crate::cache_paths::project_cache_read_subscription_rows_select()
        .select_only()
        .column(CProjectCache::Project)
        .filter(CProjectCache::Cache.eq(cache))
        .into_tuple()
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

    #[tokio::test]
    async fn project_write_caches_include_write_only_active_cache_and_read_access_requires_active_read_subscription()
     {
        use gradient_entity::project_cache::CacheSubscriptionMode::{
            ReadOnly, ReadWrite, WriteOnly,
        };
        let db = TestDatabase::create().await;
        let conn = db.connection();
        let owner = crate::rows::insert::<EUser, _>(&conn, rows::user())
            .await
            .expect("the owner is written");
        let active_cache_row = rows::cache_with_id(CacheId::now_v7(), "active", owner.id);
        let deactivated_cache_row = gradient_types::MCache {
            active: false,
            ..rows::cache_with_id(CacheId::now_v7(), "inactive", owner.id)
        };
        for cache in [&active_cache_row, &deactivated_cache_row] {
            crate::rows::insert::<ECache, _>(&conn, cache.clone())
                .await
                .expect("the cache is written");
        }
        let mut project_ids_by_subscription_mode = Vec::new();
        for (slug, cache, mode) in [
            ("write-only", &active_cache_row, WriteOnly),
            ("read-only", &active_cache_row, ReadOnly),
            ("read-write", &active_cache_row, ReadWrite),
            ("read-write-inactive", &deactivated_cache_row, ReadWrite),
        ] {
            let project = crate::rows::insert::<EProject, _>(
                &conn,
                rows::project_with_id(ProjectId::now_v7(), slug),
            )
            .await
            .expect("the project is written");
            crate::rows::insert::<EProjectCache, _>(
                &conn,
                rows::project_cache_link(ProjectCacheId::now_v7(), project.id, cache.id, mode),
            )
            .await
            .expect("the subscription is written");
            project_ids_by_subscription_mode.push(project.id);
        }
        let [write_only, read_only, read_write, read_write_inactive] =
            project_ids_by_subscription_mode[..]
        else {
            unreachable!()
        };

        let mut project_ids_reported_as_reading_cache =
            project_ids_with_read_access_to_cache(&conn, active_cache_row.id)
                .await
                .expect("the project_ids_reported_as_reading_cache are read");
        project_ids_reported_as_reading_cache.sort();
        let mut project_ids_expected_to_read_active_cache = vec![read_only, read_write];
        project_ids_expected_to_read_active_cache.sort();

        assert_eq!(
            project_ids_reported_as_reading_cache,
            project_ids_expected_to_read_active_cache
        );
        assert!(
            project_ids_with_read_access_to_cache(&conn, deactivated_cache_row.id)
                .await
                .expect("the project_ids_reported_as_reading_cache are read")
                .is_empty()
        );
        assert_eq!(
            crate::cache_paths::project_write_caches(&conn, write_only)
                .await
                .expect("Read the write caches."),
            vec![active_cache_row.id]
        );
        assert!(
            crate::cache_paths::project_write_caches(&conn, read_write_inactive)
                .await
                .expect("Read the write caches.")
                .is_empty()
        );
        db.finish().await;
    }
}
