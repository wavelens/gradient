/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Which derivations an evaluation walk may prune, answered after every queued write.

use gradient_db::WorkerDb;
use gradient_types::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

/// The prunable-derivations lookup. Any error propagates: the caller prunes nothing.
///
/// `walked` alone decides it: the bit says the subtree is recorded, which is the
/// whole contract of the walk, and it is cleared exactly where a record is lost
/// ([`gradient_db::unwalk_derivations`], the GC's orphan reclaim). Build and cache
/// state say nothing about whether the graph is recorded, so keying on them re-walked
/// a complete record for as long as its anchor had not succeeded.
pub(crate) async fn prunable(
    db: &WorkerDb,
    drv_hashes: Vec<String>,
) -> Result<Vec<String>, sea_orm::DbErr> {
    Ok(EDerivation::find()
        .filter(CDerivation::Hash.is_in(drv_hashes))
        .filter(CDerivation::Walked.eq(true))
        .all(db)
        .await?
        .into_iter()
        .map(|d| d.store_path())
        .collect())
}
