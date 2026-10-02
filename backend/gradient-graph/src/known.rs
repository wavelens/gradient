/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_db::WorkerDb;
use gradient_types::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

/// A derivation is prunable once `walked` and `unwalked_inputs = 0` both hold.
/// The second flag is keeping pruning safe against a walk abandoned between batches.
/// Build and cache state are not telling whether the graph is recorded.
/// Keying on them re-walked complete records until their shared build succeeded.
pub(crate) async fn prunable(
    db: &WorkerDb,
    drv_hashes: Vec<String>,
) -> Result<Vec<String>, sea_orm::DbErr> {
    Ok(EDerivation::find()
        .filter(CDerivation::Hash.is_in(drv_hashes))
        .filter(CDerivation::Walked.eq(true))
        .filter(CDerivation::UnwalkedInputs.eq(0))
        .all(db)
        .await?
        .into_iter()
        .map(|d| d.store_path())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::prunable;
    use gradient_db::WorkerDb;
    use sea_orm::{DatabaseBackend, MockDatabase};

    #[tokio::test]
    async fn only_a_recorded_subtree_is_prunable() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<gradient_types::MDerivation>::new()])
            .into_connection();
        let pool = WorkerDb::new(db);

        prunable(&pool, vec!["a".repeat(32)]).await.unwrap();

        let log: Vec<sea_orm::Statement> = pool
            .into_transaction_log()
            .iter()
            .flat_map(|t| t.statements().to_vec())
            .collect();
        let sql = &log[0].sql;
        assert!(
            sql.contains("\"walked\" = ") && sql.contains("\"unwalked_inputs\" = "),
            "the prune must read both bits: {sql}"
        );
    }
}
