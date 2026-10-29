/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! A shared build fetched from a cache may leave out outputs no cache holds, as long as no wanted
//! parent needs them. A parent needing one later turns the shared build into a real build.

use std::collections::{BTreeSet, HashSet};

use gradient_types::DerivationId;
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr, QueryResult, TransactionTrait};

use crate::graph::can_start::{
    ids, lock_seed_shared_builds, lock_shared_builds, lost_fetchability, promote,
    seed_blocking_deps, unpromote_ungated, update_and_settle_need,
};
use crate::graph::predicates::output_needed_predicate;
use crate::graph::runtime_can_start::{
    complete_among, reset_uncached_producers, ripple_shared_builds_incomplete,
};
use crate::scheduling::assignment_record::{build_job_key_sql, no_open_assignment_predicate};
use crate::status::TransitionChange;

fn needed_outputs_sql() -> String {
    format!(
        "SELECT o.derivation, o.name FROM derivation_output o \
         WHERE o.derivation = ANY($1::uuid[]) AND {needed}",
        needed = output_needed_predicate("o"),
    )
}

fn unneeded_outputs_now_needed_sql() -> String {
    format!(
        "SELECT db.derivation FROM derivation_build db \
         WHERE db.unneeded_outputs <> '{{}}' AND {idle} \
           AND EXISTS (SELECT 1 FROM derivation_output o \
                       WHERE o.derivation = db.derivation AND o.name = ANY(db.unneeded_outputs) \
                         AND {needed}) \
         ORDER BY db.derivation",
        idle = no_open_assignment_predicate(&build_job_key_sql("db.id")),
        needed = output_needed_predicate("o"),
    )
}

crate::sql_fn! {
    NEEDED_OUTPUTS = needed_outputs_sql,
        params = [DerivationIds(64)],
        tier = Bulk;

    UNNEEDED_OUTPUTS_NOW_NEEDED = unneeded_outputs_now_needed_sql,
        params = [],
        tier = Sweep;
}

crate::sql! {
    FORGET_UNNEEDED_OUTPUTS = "UPDATE derivation_build SET cache_available = false, unneeded_outputs = '{}', \
         updated_at = (now() AT TIME ZONE 'UTC') \
         WHERE derivation = ANY($1::uuid[]) AND unneeded_outputs <> '{}' RETURNING derivation",
        params = [DerivationIds(64)];

    BUILD_PARENTS = "SELECT DISTINCT e.derivation FROM derivation_dependency e \
         WHERE e.dependency = ANY($1::uuid[]) AND e.kind IN (0, 2) ORDER BY e.derivation",
        params = [DerivationIds(64)];
}

fn derivation_ids(rows: &[QueryResult]) -> Vec<DerivationId> {
    rows.iter()
        .filter_map(|r| r.try_get::<uuid::Uuid>("", "derivation").ok())
        .map(DerivationId::new)
        .collect()
}

pub async fn needed_outputs<C: ConnectionTrait>(
    db: &C,
    derivations: &[DerivationId],
) -> Result<HashSet<(DerivationId, String)>, DbErr> {
    if derivations.is_empty() {
        return Ok(HashSet::new());
    }

    Ok(db
        .query_all_raw(NEEDED_OUTPUTS.bind([ids(derivations)]))
        .await?
        .iter()
        .filter_map(|r| {
            let derivation = r.try_get::<uuid::Uuid>("", "derivation").ok()?;
            let name = r.try_get::<String>("", "name").ok()?;
            Some((DerivationId::new(derivation), name))
        })
        .collect())
}

pub async fn unneeded_outputs_now_needed<C: ConnectionTrait>(
    db: &C,
) -> Result<Vec<DerivationId>, DbErr> {
    Ok(derivation_ids(
        &db.query_all_raw(UNNEEDED_OUTPUTS_NOW_NEEDED.stmt()).await?,
    ))
}

pub async fn forget_unneeded_outputs<C>(
    db: &C,
    derivations: &[DerivationId],
) -> Result<Vec<TransitionChange>, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    if derivations.is_empty() {
        return Ok(Vec::new());
    }

    let txn = db.begin().await?;
    let _lock = lock_shared_builds(&txn, derivations).await?;
    let was_complete: BTreeSet<DerivationId> = complete_among(&txn, derivations)
        .await?
        .into_iter()
        .collect();
    let forgotten = derivation_ids(
        &txn.query_all_raw(FORGET_UNNEEDED_OUTPUTS.bind([ids(derivations)]))
            .await?,
    );
    if forgotten.is_empty() {
        txn.commit().await?;
        return Ok(Vec::new());
    }

    let mut changes = settle_forgotten(&txn, &forgotten, &was_complete).await?;
    txn.commit().await?;
    changes.extend(update_and_settle_need(db, &forgotten).await?.changes);

    Ok(changes)
}

async fn settle_forgotten(
    txn: &DatabaseTransaction,
    forgotten: &[DerivationId],
    was_complete: &BTreeSet<DerivationId>,
) -> Result<Vec<TransitionChange>, DbErr> {
    let incomplete = lost_completeness(txn, forgotten, was_complete).await?;
    let mut scope = forgotten.to_vec();
    scope.extend(incomplete);
    scope.sort_unstable();
    scope.dedup();

    let lock = lock_shared_builds(txn, &scope).await?;
    let mut changes = lost_fetchability(&lock).await?;
    changes.extend(reset_uncached_producers(txn, forgotten).await?);
    changes.extend(unpromote_ungated(txn, forgotten).await?);
    changes.extend(recount_build_parents(txn, forgotten).await?);

    Ok(changes)
}

async fn lost_completeness(
    txn: &DatabaseTransaction,
    forgotten: &[DerivationId],
    was_complete: &BTreeSet<DerivationId>,
) -> Result<Vec<DerivationId>, DbErr> {
    let still: BTreeSet<DerivationId> = complete_among(txn, forgotten).await?.into_iter().collect();
    let lost: Vec<DerivationId> = forgotten
        .iter()
        .copied()
        .filter(|d| was_complete.contains(d) && !still.contains(d))
        .collect();

    let mut incomplete = lost.clone();
    incomplete.extend(ripple_shared_builds_incomplete(txn, lost).await?);

    Ok(incomplete)
}

/// The ripples move parents only over edges free of unneeded outputs, so the parents of a
/// shared build whose unneeded outputs are forgotten are counted again from scratch.
async fn recount_build_parents(
    txn: &DatabaseTransaction,
    forgotten: &[DerivationId],
) -> Result<Vec<TransitionChange>, DbErr> {
    let parents = derivation_ids(
        &txn.query_all_raw(BUILD_PARENTS.bind([ids(forgotten)]))
            .await?,
    );
    if parents.is_empty() {
        return Ok(Vec::new());
    }

    let lock = lock_seed_shared_builds(txn, &parents).await?;
    seed_blocking_deps(&lock).await?;
    let mut changes = unpromote_ungated(txn, &parents).await?;
    changes.extend(promote(txn, &parents).await?);

    Ok(changes)
}
