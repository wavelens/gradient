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
    use gradient_fixtures::mock::{all_statements, mock_db};

    #[tokio::test]
    async fn only_a_recorded_subtree_is_prunable() {
        let db = mock_db()
            .append_query_results([Vec::<gradient_types::MDerivation>::new()])
            .into_connection();
        let pool = WorkerDb::new(db);

        prunable(&pool, vec!["a".repeat(32)]).await.unwrap();

        let log: Vec<sea_orm::Statement> = all_statements(pool.into_transaction_log());
        let sql = &log[0].sql;
        assert!(
            sql.contains("\"walked\" = ") && sql.contains("\"unwalked_inputs\" = "),
            "the prune must read both bits: {sql}"
        );
    }
}
