/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::{
    build_attempt, build_job, cache, cache_upstream, cached_path, cluster_attempt, cluster_job,
    cluster_member, commit, derivation, derivation_build, derivation_dependency,
    derivation_feature, derivation_metric, derivation_output, dispatched_job, dispatched_job_phase,
    entry_point, eval_cache_store, eval_split, evaluation, evaluation_flake_input_override,
    evaluation_input_update, evaluation_metric, feature, project, project_cache, task, task_action,
    task_flake_input_override, task_trigger, user, worker_registration,
};
use sea_orm::sea_query::OnConflict;
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
    if let Some(owner) = shape.owner {
        user::Entity::insert(owner.into_active_model())
            .on_conflict(OnConflict::column(user::Column::Id).do_nothing().to_owned())
            .exec_without_returning(db)
            .await
            .unwrap_or_else(|error| panic!("user::Entity: {error}"));
    }
    insert!(db, project::Entity, shape.projects);
    insert!(db, task::Entity, shape.tasks);
    insert!(db, task_trigger::Entity, shape.triggers);
    insert!(db, task_action::Entity, shape.actions);
    insert!(db, task_flake_input_override::Entity, shape.tracked_inputs);
    insert!(db, commit::Entity, shape.commits);
    insert!(db, evaluation::Entity, shape.evaluations);
    insert!(db, derivation::Entity, shape.derivations);
    insert!(db, derivation_build::Entity, shape.shared_builds);
    insert!(db, derivation_dependency::Entity, shape.edges);
    insert!(db, cached_path::Entity, shape.cached_paths);
    insert!(db, derivation_output::Entity, shape.outputs);
    insert!(db, feature::Entity, shape.features);
    insert!(db, derivation_feature::Entity, shape.derivation_features);
    insert!(db, build_job::Entity, shape.jobs);
    insert!(db, entry_point::Entity, shape.entry_points);
    insert!(db, cache::Entity, shape.caches);
    insert!(db, project_cache::Entity, shape.project_caches);
    insert!(db, cache_upstream::Entity, shape.upstreams);
    insert!(db, cluster_job::Entity, shape.clusters);
    insert!(db, cluster_member::Entity, shape.members);
    insert!(db, eval_split::Entity, shape.eval_splits);
    insert!(db, cluster_attempt::Entity, shape.cluster_attempts);
    insert!(db, dispatched_job::Entity, shape.assignments);
    insert!(db, dispatched_job_phase::Entity, shape.phases);
    insert!(db, build_attempt::Entity, shape.attempts);
    insert!(
        db,
        evaluation_flake_input_override::Entity,
        shape.input_overrides
    );
    insert!(db, evaluation_input_update::Entity, shape.input_updates);
    insert!(db, derivation_metric::Entity, shape.derivation_metrics);
    insert!(db, evaluation_metric::Entity, shape.evaluation_metrics);
    insert!(db, eval_cache_store::Entity, shape.eval_caches);
    insert!(db, worker_registration::Entity, shape.registrations);
}
