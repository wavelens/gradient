/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::restart_build_status;
use crate::trigger::TriggerError;
use gradient_types::*;
use chrono::NaiveDateTime;
use gradient_entity::build::BuildStatus;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ConnectionTrait};
use std::collections::HashMap;

/// Inserts the restart's builds, mapping each previous build's status through
/// [`restart_build_status`] and following any active cross-evaluation leader so
/// two evaluations never race for the same Nix store lock. Returns the
/// old→new build id map used to remap entry points.
pub(super) async fn create_restart_builds<C: ConnectionTrait>(
    db: &C,
    project: &MProject,
    new_eval_id: EvaluationId,
    prev_builds: &[MBuild],
    now: NaiveDateTime,
) -> Result<HashMap<BuildId, BuildId>, TriggerError> {
    // Look up any in-flight leader (Created/Queued/Building) in another
    // evaluation for the derivations we're about to rebuild. Restarting must
    // honour the same cross-evaluation dedup as the regular eval-result path,
    // otherwise two projects in the same organisation race for the Nix store
    // lock when one restarts while the other is still building.
    let queued_drv_ids: Vec<DerivationId> = prev_builds
        .iter()
        .filter(|b| !matches!(restart_build_status(b.status), BuildStatus::Substituted))
        .map(|b| b.derivation)
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    let leader_for_drv =
        gradient_db::find_active_leaders(db, project.organization, &queued_drv_ids).await?;

    let mut build_id_map: HashMap<BuildId, BuildId> = HashMap::with_capacity(prev_builds.len());

    for prev_build in prev_builds {
        let new_status = restart_build_status(prev_build.status);
        let new_build_id = BuildId::now_v7();
        let via = if matches!(new_status, BuildStatus::Substituted) {
            None
        } else {
            leader_for_drv.get(&prev_build.derivation).copied()
        };
        let abuild = ABuild {
            id: Set(new_build_id),
            evaluation: Set(new_eval_id),
            derivation: Set(prev_build.derivation),
            status: Set(new_status),
            via: Set(via),
            substitutable: Set(false),
            attempt: Set(0),
            timeout_secs: Set(prev_build.timeout_secs),
            max_silent_secs: Set(prev_build.max_silent_secs),
            prefer_local_build: Set(prev_build.prefer_local_build),
            created_at: Set(now),
            updated_at: Set(now),
            queued_at: Set((new_status == BuildStatus::Queued).then_some(now)),
            ready_at: Set(None),
            dispatched_at: Set(None),
        };
        abuild.insert(db).await?;
        build_id_map.insert(prev_build.id, new_build_id);
    }

    Ok(build_id_map)
}
