/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::authorization::{MaybeApiKey, MaybeUser};
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use axum::extract::{Path, State};
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_db::scheduling::build_attempt::latest_attempt;
use gradient_sources::get_path_from_derivation_output;
use gradient_types::*;
use gradient_util::latest::Latest;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use super::BuildAccessContext;

#[derive(Serialize, Deserialize, Debug)]
pub struct BuildWithOutputs {
    pub id: BuildJobId,
    pub evaluation: EvaluationId,
    pub status: gradient_entity::build::BuildStatus,
    pub derivation_path: String,
    pub architecture: gradient_entity::server::Architecture,
    pub worker: Option<String>,
    pub dispatched_job: Option<DispatchedJobId>,
    pub output: HashMap<String, String>,
    pub prioritized: bool,
    pub created_at: chrono::NaiveDateTime,
    pub updated_at: chrono::NaiveDateTime,
    pub progress: Option<BuildProgress>,
}

/// A finished shared build's last report is outliving it by up to the TTL. It is not shown.
fn running_progress(
    progress: &Latest<DerivationBuildId, BuildProgress>,
    shared_build: DerivationBuildId,
    status: gradient_entity::build::BuildStatus,
    now: Instant,
) -> Option<BuildProgress> {
    (status == gradient_entity::build::BuildStatus::Building)
        .then(|| progress.get(&shared_build, now))
        .flatten()
}

pub async fn get_build(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(build_id): Path<BuildJobId>,
) -> WebResult<Json<BaseResponse<BuildWithOutputs>>> {
    let ctx = BuildAccessContext::load(&state, build_id, &maybe_user, api_key.as_ref()).await?;
    let build_job = ctx.build_job;
    let shared_build = ctx.shared_build;

    let derivation = EDerivation::find_by_id(build_job.derivation)
        .one(&state.web_db)
        .await?
        .ok_or_else(|| {
            tracing::warn!(
                derivation_id = %build_job.derivation,
                %build_id,
                "Derivation not found for build"
            );
            WebError::data_inconsistency("Build")
        })?;

    let derivation_outputs = EDerivationOutput::find()
        .filter(CDerivationOutput::Derivation.eq(derivation.id))
        .all(&state.web_db)
        .await?;

    let mut outputs = HashMap::new();
    for output in derivation_outputs {
        let path = get_path_from_derivation_output(output.clone()).base();
        outputs.insert(output.name, path);
    }

    let attempt = latest_attempt(&state.web_db, shared_build.id)
        .await
        .ok()
        .flatten();
    let worker = match &attempt {
        Some(a) => gradient_entity::dispatched_job::Entity::find_by_id(a.dispatched_job)
            .one(&state.web_db)
            .await?
            .map(|j| j.worker_id),
        None => None,
    };

    let build_with_outputs = BuildWithOutputs {
        id: build_job.id,
        evaluation: build_job.evaluation,
        status: shared_build.status.for_api(),
        derivation_path: derivation.drv_path(),
        architecture: derivation.architecture,
        worker,
        dispatched_job: attempt.map(|a| a.dispatched_job),
        output: outputs,
        prioritized: shared_build.prioritized,
        created_at: build_job.created_at,
        updated_at: shared_build.updated_at,
        progress: running_progress(
            &state.build_progress,
            shared_build.id,
            shared_build.status,
            Instant::now(),
        ),
    };

    Ok(ok_json(build_with_outputs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::build::BuildStatus;

    #[test]
    fn only_a_building_shared_build_shows_its_progress() {
        let latest = Latest::new(std::time::Duration::from_secs(10));
        let shared_build = DerivationBuildId::now_v7();
        let now = Instant::now();
        let progress = BuildProgress {
            phase: BuildProgressPhase::Upload,
            bytes_done: 1,
            bytes_total: Some(2),
            paths_done: 0,
            paths_total: Some(1),
        };
        latest.set(shared_build, progress, now);

        assert_eq!(
            running_progress(&latest, shared_build, BuildStatus::Building, now),
            Some(progress)
        );
        assert_eq!(
            running_progress(&latest, shared_build, BuildStatus::Completed, now),
            None
        );
    }
}
