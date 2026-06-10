/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::build_status::update_build_status;
use super::evaluation_status::update_evaluation_status;
use super::leader_election::reelect_leader;
use crate::DbContext;
use gradient_types::*;
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::EvaluationStatus;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, Condition, EntityTrait, IntoActiveModel, QueryFilter};
use tracing::error;

pub async fn abort_evaluation(ctx: &DbContext, evaluation: MEvaluation) {
    if evaluation.status == EvaluationStatus::Completed {
        return;
    }

    let builds = match EBuild::find()
        .filter(CBuild::Evaluation.eq(evaluation.id))
        .filter(
            Condition::any()
                .add(CBuild::Status.eq(BuildStatus::Created))
                .add(CBuild::Status.eq(BuildStatus::Queued))
                .add(CBuild::Status.eq(BuildStatus::Building)),
        )
        .all(&ctx.worker_db)
        .await
    {
        Ok(builds) => builds,
        Err(e) => {
            error!(error = %e, evaluation_id = %evaluation.id, "Failed to query builds for evaluation abort");
            return;
        }
    };

    for build in builds {
        if build.via.is_some() {
            // Follower: aborting it does not interrupt the leader's work in
            // another evaluation. Clear `via` so the eventual leader-completion
            // sweep skips it, then mark Aborted.
            abort_follower(ctx, build).await;
            continue;
        }

        // Leader (or plain build).
        let has_followers = match EBuild::find()
            .filter(CBuild::Via.eq(build.id))
            .one(&ctx.worker_db)
            .await
        {
            Ok(opt) => opt.is_some(),
            Err(e) => {
                error!(error = %e, build_id = %build.id, "Failed to query followers for abort");
                false
            }
        };

        if has_followers && build.status == BuildStatus::Building {
            // Already running on a worker - let it finish so followers in
            // other (non-aborted) evaluations get the result.
            continue;
        }

        if has_followers && matches!(build.status, BuildStatus::Queued | BuildStatus::Created) {
            // Hand off leadership before aborting.
            if let Err(e) = reelect_leader(ctx, &build).await {
                error!(error = %e, build_id = %build.id, "Failed to re-elect leader on abort");
            }
        }

        update_build_status(ctx, build, BuildStatus::Aborted).await;
    }

    update_evaluation_status(ctx, evaluation, EvaluationStatus::Aborted).await;
}

async fn abort_follower(ctx: &DbContext, build: MBuild) {
    let mut active: ABuild = build.clone().into_active_model();
    active.via = Set(None);
    if let Err(e) = active.update(&ctx.worker_db).await {
        error!(error = %e, build_id = %build.id, "Failed to clear via on follower abort");
        return;
    }
    let reloaded = match EBuild::find_by_id(build.id).one(&ctx.worker_db).await {
        Ok(Some(b)) => b,
        Ok(None) => return,
        Err(e) => {
            error!(error = %e, build_id = %build.id, "Failed to reload follower for abort");
            return;
        }
    };
    update_build_status(ctx, reloaded, BuildStatus::Aborted).await;
}
