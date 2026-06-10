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

/// Promote one same-org follower of `leader` to be the new leader.
/// Cross-org followers have their `via` cleared (made independent).
/// No-op if no followers exist.
pub(crate) async fn reelect_leader(ctx: &DbContext, leader: &MBuild) -> Result<(), sea_orm::DbErr> {
    use gradient_entity::derivation::{Column as CDerivation, Entity as EDerivation};

    let leader_org = EDerivation::find_by_id(leader.derivation)
        .one(&ctx.worker_db)
        .await?
        .map(|d| d.organization);

    let all_followers = EBuild::find()
        .filter(CBuild::Via.eq(leader.id))
        .all(&ctx.worker_db)
        .await?;
    if all_followers.is_empty() {
        return Ok(());
    }

    let follower_drv_ids: Vec<DerivationId> = all_followers.iter().map(|f| f.derivation).collect();
    let drv_org: std::collections::HashMap<DerivationId, OrganizationId> =
        crate::fetch_in_chunks(&follower_drv_ids, |chunk| async move {
            EDerivation::find()
                .filter(CDerivation::Id.is_in(chunk))
                .all(&ctx.worker_db)
                .await
        })
        .await?
        .into_iter()
        .map(|d| (d.id, d.organization))
        .collect();

    let mut same_org: Vec<MBuild> = Vec::new();
    let mut cross_org: Vec<MBuild> = Vec::new();
    for f in all_followers {
        let org = drv_org.get(&f.derivation).copied();
        if org == leader_org && org.is_some() {
            same_org.push(f);
        } else {
            cross_org.push(f);
        }
    }

    fn rank(s: BuildStatus) -> u8 {
        match s {
            BuildStatus::Building => 2,
            BuildStatus::Queued => 1,
            _ => 0,
        }
    }
    same_org.sort_by(|a, b| {
        rank(b.status)
            .cmp(&rank(a.status))
            .then_with(|| a.created_at.cmp(&b.created_at))
    });

    if let Some(new_leader) = same_org.first().cloned() {
        let mut active: ABuild = new_leader.clone().into_active_model();
        active.via = Set(None);
        active.update(&ctx.worker_db).await?;

        let same_org_remaining_ids: Vec<BuildId> = same_org.iter().skip(1).map(|f| f.id).collect();
        crate::for_each_chunk(&same_org_remaining_ids, |chunk| async move {
            EBuild::update_many()
                .col_expr(CBuild::Via, sea_orm::sea_query::Expr::value(new_leader.id))
                .filter(CBuild::Id.is_in(chunk))
                .exec(&ctx.worker_db)
                .await
        })
        .await?;

        let cross_org_ids: Vec<BuildId> = cross_org.iter().map(|f| f.id).collect();
        crate::for_each_chunk(&cross_org_ids, |chunk| async move {
            EBuild::update_many()
                .col_expr(
                    CBuild::Via,
                    sea_orm::sea_query::Expr::value(Option::<BuildId>::None),
                )
                .filter(CBuild::Id.is_in(chunk))
                .exec(&ctx.worker_db)
                .await
        })
        .await?;

        debug!(
            old_leader = %leader.id,
            new_leader = %new_leader.id,
            cross_org_orphaned = cross_org.len(),
            "re-elected build leader (same-org), cross-org followers made independent"
        );
        return Ok(());
    }

    let cross_org_ids: Vec<BuildId> = cross_org.iter().map(|f| f.id).collect();
    if !cross_org_ids.is_empty() {
        crate::for_each_chunk(&cross_org_ids, |chunk| async move {
            EBuild::update_many()
                .col_expr(
                    CBuild::Via,
                    sea_orm::sea_query::Expr::value(Option::<BuildId>::None),
                )
                .filter(CBuild::Id.is_in(chunk))
                .exec(&ctx.worker_db)
                .await
        })
        .await?;
        debug!(
            old_leader = %leader.id,
            orphaned = cross_org.len(),
            "leader aborted with no same-org followers; cross-org followers made independent"
        );
    }
    Ok(())
}

/// For each derivation in `drv_ids`, return the id of the leader build whose
/// result a new build for that derivation should follow.
///
/// First checks for an in-flight build within `inserting_org`. When no
/// same-org candidate exists for a drv, consults cache-connected organisations
/// via [`cache_reach::writer_orgs_reachable_from`](crate::cache_reach::writer_orgs_reachable_from)
/// and picks the most-advanced active build (tie-break: oldest `created_at`).
///
/// Drvs with no active build are omitted from the result.
pub async fn find_active_leaders<C: ConnectionTrait>(
    db: &C,
    inserting_org: OrganizationId,
    drv_ids: &[DerivationId],
) -> Result<HashMap<DerivationId, BuildId>, sea_orm::DbErr> {
    if drv_ids.is_empty() {
        return Ok(HashMap::new());
    }

    // ── Same-org pass ────────────────────────────────────────────────────
    let same_org_rows = crate::fetch_in_chunks(drv_ids, |chunk| async move {
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
    for b in same_org_rows {
        let head = b.via.unwrap_or(b.id);
        out.entry(b.derivation)
            .and_modify(|cur| {
                if b.via.is_none() {
                    *cur = b.id;
                }
            })
            .or_insert(head);
    }

    let unmatched: Vec<DerivationId> = drv_ids
        .iter()
        .copied()
        .filter(|d| !out.contains_key(d))
        .collect();
    if unmatched.is_empty() {
        return Ok(out);
    }

    // ── Cross-org pass ───────────────────────────────────────────────────
    use gradient_entity::derivation::{Column as CDerivation, Entity as EDerivation};

    let inserting_drv_rows = crate::fetch_in_chunks(&unmatched, |chunk| async move {
        EDerivation::find()
            .filter(CDerivation::Id.is_in(chunk))
            .all(db)
            .await
    })
    .await?;
    let mut path_to_drv: HashMap<String, DerivationId> = HashMap::new();
    let mut drv_hashes: Vec<String> = Vec::new();
    for d in &inserting_drv_rows {
        path_to_drv.insert(d.drv_path(), d.id);
        drv_hashes.push(d.hash.clone());
    }
    if drv_hashes.is_empty() {
        return Ok(out);
    }

    let mut reachable =
        crate::cache_reach::writer_orgs_reachable_from(db, inserting_org).await?;
    reachable.remove(&inserting_org);
    if reachable.is_empty() {
        return Ok(out);
    }

    let reachable_orgs: Vec<_> = reachable.into_iter().collect();
    let candidate_drvs = crate::fetch_in_chunks(&drv_hashes, |chunk| {
        let reachable_orgs = reachable_orgs.clone();
        async move {
            EDerivation::find()
                .filter(CDerivation::Hash.is_in(chunk))
                .filter(CDerivation::Organization.is_in(reachable_orgs))
                .all(db)
                .await
        }
    })
    .await?;
    if candidate_drvs.is_empty() {
        return Ok(out);
    }
    let candidate_drv_ids: Vec<DerivationId> = candidate_drvs.iter().map(|d| d.id).collect();
    let leader_drv_to_path: HashMap<DerivationId, String> = candidate_drvs
        .into_iter()
        .map(|d| (d.id, d.drv_path()))
        .collect();

    let candidate_builds = crate::fetch_in_chunks(&candidate_drv_ids, |chunk| async move {
        EBuild::find()
            .filter(CBuild::Derivation.is_in(chunk))
            .filter(CBuild::Status.is_in(vec![
                BuildStatus::Created,
                BuildStatus::Queued,
                BuildStatus::Building,
            ]))
            .filter(CBuild::Via.is_null())
            .filter(CBuild::Substitutable.eq(false))
            .all(db)
            .await
    })
    .await?;

    fn status_rank(s: BuildStatus) -> u8 {
        match s {
            BuildStatus::Building => 2,
            BuildStatus::Queued => 1,
            _ => 0,
        }
    }
    let mut best_by_path: HashMap<String, MBuild> = HashMap::new();
    for b in candidate_builds {
        let Some(path) = leader_drv_to_path.get(&b.derivation).cloned() else {
            continue;
        };
        match best_by_path.get(&path) {
            Some(cur) => {
                let cur_rank = status_rank(cur.status);
                let new_rank = status_rank(b.status);
                if new_rank > cur_rank || (new_rank == cur_rank && b.created_at < cur.created_at) {
                    best_by_path.insert(path, b);
                }
            }
            None => {
                best_by_path.insert(path, b);
            }
        }
    }

    for (path, b) in best_by_path {
        if let Some(&local_drv_id) = path_to_drv.get(&path) {
            out.insert(local_drv_id, b.id);
        }
    }

    Ok(out)
}
