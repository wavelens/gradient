/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_db::WorkerDb;
use gradient_types::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

/// A derivation is prunable if both `walked` and `unwalked_inputs = 0` are true and a verified hash modulo is present in the row. The closure check for dependent derivations can't get input from that row if that value is absent. Evaluation workers report the derivation again.
/// The second flag is keeping pruning safe against a walk abandoned between batches.
/// Build and cache state are not telling whether the graph is recorded.
/// Keying on them re-walked complete records until their shared build succeeded.
/// `walked` can also mean that the cache can provide the `.drv` file and its input sources.
/// Retiring either one can clear `walked`. Later evaluations parse the derivation again and push them.
pub(crate) async fn prunable(
    db: &WorkerDb,
    drv_hashes: Vec<String>,
) -> Result<Vec<String>, sea_orm::DbErr> {
    Ok(EDerivation::find()
        .filter(CDerivation::Hash.is_in(drv_hashes))
        .filter(CDerivation::Walked.eq(true))
        .filter(CDerivation::UnwalkedInputs.eq(0))
        .filter(CDerivation::SerializedDerivationHashForDependentOutputPaths.is_not_null())
        .all(db)
        .await?
        .into_iter()
        .map(|d| d.store_path())
        .collect())
}
