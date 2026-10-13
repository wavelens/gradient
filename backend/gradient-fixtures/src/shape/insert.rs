/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::{
    build_job, cache, cached_path, commit, derivation, derivation_build, derivation_dependency,
    derivation_output, entry_point, evaluation, project, project_cache, task, user,
};
use sea_orm::{DatabaseConnection, EntityTrait, IntoActiveModel};

use super::Shape;

macro_rules! insert {
    ($db:expr, $entity:ty, $rows:expr) => {
        let rows = $rows;
        if !rows.is_empty() {
            <$entity>::insert_many(rows.into_iter().map(IntoActiveModel::into_active_model))
                .exec_without_returning($db)
                .await
                .unwrap_or_else(|error| panic!("{}: {error}", stringify!($entity)));
        }
    };
}

pub(super) async fn all(shape: Shape, db: &DatabaseConnection) {
    insert!(
        db,
        user::Entity,
        shape.owner.into_iter().collect::<Vec<_>>()
    );
    insert!(db, project::Entity, shape.projects);
    insert!(db, task::Entity, shape.tasks);
    insert!(db, commit::Entity, shape.commits);
    insert!(db, evaluation::Entity, shape.evaluations);
    insert!(db, derivation::Entity, shape.derivations);
    insert!(db, derivation_build::Entity, shape.shared_builds);
    insert!(db, derivation_dependency::Entity, shape.edges);
    insert!(db, cached_path::Entity, shape.cached_paths);
    insert!(db, derivation_output::Entity, shape.outputs);
    insert!(db, build_job::Entity, shape.jobs);
    insert!(db, entry_point::Entity, shape.entry_points);
    insert!(db, cache::Entity, shape.caches);
    insert!(db, project_cache::Entity, shape.project_caches);
}
