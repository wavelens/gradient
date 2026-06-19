/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::DbContext;
use gradient_types::*;
use gradient_entity::build::BuildStatus;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, IntoActiveModel, QueryFilter};
use std::collections::HashMap;
use tracing::debug;

fn rank(s: BuildStatus) -> u8 {
    match s {
        BuildStatus::Building => 2,
        BuildStatus::Queued => 1,
        _ => 0,
    }
}

/// Promote the most-advanced follower of `leader` to be the new leader and
/// re-point the rest at it. Derivations are global, so all followers share the
/// same derivation; no org split. No-op if no followers exist.
pub(crate) async fn reelect_leader(ctx: &DbContext, leader: &MBuild) -> Result<(), sea_orm::DbErr> {
    let mut followers = EBuild::find()
        .filter(CBuild::Via.eq(leader.id))
        .all(&ctx.worker_db)
        .await?;
    if followers.is_empty() {
        return Ok(());
    }

    followers.sort_by(|a, b| {
        rank(b.status)
            .cmp(&rank(a.status))
            .then_with(|| a.created_at.cmp(&b.created_at))
    });

    let new_leader_id = followers[0].id;
    let mut active: ABuild = followers[0].clone().into_active_model();
    active.via = Set(None);
    active.update(&ctx.worker_db).await?;

    let remaining: Vec<BuildId> = followers.iter().skip(1).map(|f| f.id).collect();
    crate::for_each_chunk(&remaining, |chunk| async move {
        EBuild::update_many()
            .col_expr(CBuild::Via, sea_orm::sea_query::Expr::value(new_leader_id))
            .filter(CBuild::Id.is_in(chunk))
            .exec(&ctx.worker_db)
            .await
    })
    .await?;

    debug!(old_leader = %leader.id, new_leader = %new_leader_id, "re-elected build leader");
    Ok(())
}

/// For each derivation in `drv_ids`, return the leader build a new build should
/// follow. Derivations are global, so a single lookup over all active builds for
/// the derivation suffices. Drvs with no active build are omitted.
pub async fn find_active_leaders<C: ConnectionTrait>(
    db: &C,
    _inserting_org: OrganizationId,
    drv_ids: &[DerivationId],
) -> Result<HashMap<DerivationId, BuildId>, sea_orm::DbErr> {
    if drv_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let rows = crate::fetch_in_chunks(drv_ids, |chunk| async move {
        EBuild::find()
            .filter(CBuild::Derivation.is_in(chunk))
            .filter(CBuild::Status.is_in(vec![
                BuildStatus::Created,
                BuildStatus::Queued,
                BuildStatus::Building,
            ]))
            .all(db)
            .await
    })
    .await?;

    let mut out: HashMap<DerivationId, BuildId> = HashMap::new();
    for b in rows {
        let head = b.via.unwrap_or(b.id);
        out.entry(b.derivation)
            .and_modify(|cur| {
                if b.via.is_none() {
                    *cur = b.id;
                }
            })
            .or_insert(head);
    }

    Ok(out)
}
