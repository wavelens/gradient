/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

/// Visibility rules match `load_*(Readable)`. Superusers can't bypass them. Placeholder arguments
/// keep the public list flag. Stars from users keep public items visible regardless of that flag.
macro_rules! project_readable {
    ($listed:literal) => {
        concat!(
            "(((",
            $listed,
            " OR EXISTS (SELECT 1 FROM user_project_star ps WHERE ps.\"user\" = $1 AND ps.project = p.id)) \
            AND p.public) OR EXISTS (SELECT 1 FROM project_access pu \
            WHERE pu.project = p.id AND pu.\"user\" = $1))"
        )
    };
}

macro_rules! user_can_see_cache_sql {
    () => {
        "(EXISTS (SELECT 1 FROM cache_access cu WHERE cu.cache = c.id AND cu.\"user\" = $1) \
        OR EXISTS (SELECT 1 FROM project_cache pc JOIN project_access pu ON pu.project = pc.project \
        WHERE pc.cache = c.id AND pu.\"user\" = $1))"
    };
}

macro_rules! cache_readable {
    ($listed:literal) => {
        concat!(
            "(((",
            $listed,
            " OR EXISTS (SELECT 1 FROM user_cache_star cs WHERE cs.\"user\" = $1 AND cs.cache = c.id)) \
            AND c.public) OR ",
            user_can_see_cache_sql!(),
            ")"
        )
    };
}

use gradient_types::UserId;
use sea_orm::Value;

use crate::evaluations::current_evaluation::NON_PR;

mod rail;
mod search;
mod stars;
mod tasks;
mod totals;

pub use rail::{RailCacheRow, RailProjectRow, RailTaskRow, rail_caches, rail_projects, rail_tasks};
pub use search::{
    CommitHitRow, NameHitRow, NameKind, NarHitRow, ilike_contains, readable_cache_ids,
    search_commits, search_names, search_nars,
};
pub use stars::{StarKind, StarredNames, StarredTask, star, starred, starred_names, unstar};
pub use tasks::{
    HistoryRow, LatestTaskEvaluationRow, TaskFactsRow, entry_point_outcomes, histories, task_facts,
};
pub use totals::{
    ActivityDay, Totals, activity, activity_sql, cache_size, scoped_totals, totals_sql,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Viewer {
    pub user: UserId,
    pub public_listing: bool,
}

impl Viewer {
    fn user_value(self) -> Value {
        stars::user_value(self.user)
    }

    fn listing_value(self) -> Value {
        Value::Bool(Some(self.public_listing))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::database::TestDatabase;
    use gradient_fixtures::rows;
    use gradient_types::*;
    use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};

    async fn rail_cache_names_for_user(
        conn: &sea_orm::DatabaseConnection,
        user: UserId,
    ) -> Vec<String> {
        rail_caches(conn, user)
            .await
            .expect("the rail query runs")
            .into_iter()
            .map(|row| row.name)
            .collect()
    }

    #[tokio::test]
    async fn cache_creator_without_role_does_not_see_cache_or_count_it_in_totals() {
        let db = TestDatabase::create().await;
        let conn = db.connection();
        let creator = crate::rows::insert::<EUser, _>(&conn, rows::user())
            .await
            .expect("the user is written");
        let cache = crate::rows::insert::<ECache, _>(&conn, rows::cache())
            .await
            .expect("the cache is written");
        assert_eq!(cache.created_by, creator.id);
        rows::insert_cache_creator_admin(&conn, cache.id, creator.id)
            .await
            .expect("the creator role is written");
        conn.execute_unprepared(&format!(
            "INSERT INTO cache_usage (cache, bytes) VALUES ('{}', 42)",
            cache.id.into_inner()
        ))
        .await
        .expect("the cache usage is written");

        assert_eq!(
            rail_cache_names_for_user(&conn, creator.id).await,
            ["test-cache"]
        );
        assert_eq!(cache_size(&conn, creator.id, false).await.unwrap(), 42);

        ECacheUser::delete_many()
            .filter(CCacheUser::User.eq(creator.id))
            .exec(&conn)
            .await
            .expect("the creator role is removed");

        assert!(
            rail_cache_names_for_user(&conn, creator.id)
                .await
                .is_empty()
        );
        assert_eq!(cache_size(&conn, creator.id, false).await.unwrap(), 0);
        assert_eq!(cache_size(&conn, creator.id, true).await.unwrap(), 42);
        db.finish().await;
    }

    async fn sorted_cache_names_visible_to_user_in_cache_list(
        conn: &sea_orm::DatabaseConnection,
        user: UserId,
    ) -> Vec<String> {
        let mut names: Vec<String> = ECache::find()
            .filter(crate::caches::visible_to_user(user))
            .all(conn)
            .await
            .expect("the cache listing query runs")
            .into_iter()
            .map(|cache| cache.name)
            .collect();
        names.sort();
        names
    }

    #[tokio::test]
    async fn dashboard_side_panel_and_cache_list_show_same_private_caches_for_every_user() {
        let db = TestDatabase::create().await;
        let conn = db.connection();
        let mut users = Vec::new();
        for name in ["cache-role-holder", "project-member", "outsider", "both"] {
            let user = crate::rows::insert::<EUser, _>(
                &conn,
                gradient_entity::user::Model {
                    id: UserId::now_v7(),
                    username: name.into(),
                    email: format!("{name}@example.com"),
                    ..rows::user()
                },
            )
            .await
            .expect("the user is written");
            users.push(user.id);
        }
        let [cache_role_holder, project_member, _, both] = users[..] else {
            unreachable!()
        };
        let mut caches = Vec::new();
        for name in ["direct-cache", "subscribed-cache", "unreachable-cache"] {
            let cache = crate::rows::insert::<ECache, _>(
                &conn,
                rows::cache_with_id(CacheId::now_v7(), name, cache_role_holder),
            )
            .await
            .expect("the cache is written");
            caches.push(cache.id);
        }
        let project = crate::rows::insert::<EProject, _>(
            &conn,
            gradient_entity::project::Model {
                created_by: cache_role_holder,
                ..rows::project_with_id(ProjectId::now_v7(), "subscribing-project")
            },
        )
        .await
        .expect("the project is written");
        crate::rows::insert::<EProjectCache, _>(
            &conn,
            rows::project_cache_link(
                ProjectCacheId::now_v7(),
                project.id,
                caches[1],
                gradient_entity::project_cache::CacheSubscriptionMode::ReadOnly,
            ),
        )
        .await
        .expect("the subscription is written");
        crate::rows::insert::<ERole, _>(&conn, rows::view_role())
            .await
            .expect("the view role is written");
        for user in [project_member, both] {
            crate::rows::insert::<EProjectUser, _>(
                &conn,
                gradient_entity::project_user::Model {
                    project: project.id,
                    user,
                    ..rows::project_member(BASE_ROLE_VIEW_ID)
                },
            )
            .await
            .expect("the project membership is written");
        }
        for user in [cache_role_holder, both] {
            rows::insert_cache_creator_admin(&conn, caches[0], user)
                .await
                .expect("the cache role is written");
        }

        for user in users {
            let mut dashboard = rail_cache_names_for_user(&conn, user).await;
            dashboard.sort();
            assert_eq!(
                dashboard,
                sorted_cache_names_visible_to_user_in_cache_list(&conn, user).await
            );
        }
        assert_eq!(
            sorted_cache_names_visible_to_user_in_cache_list(&conn, both).await,
            ["direct-cache", "subscribed-cache"]
        );
        db.finish().await;
    }
}
