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
/// A derivation is prunable when its subtree is recorded: `walked` says its own
/// record is in, `unwalked_inputs = 0` says every input's is too, transitively. The
/// second bit is what makes the first one safe against a walk abandoned between
/// batches; [`gradient_db::walk_completeness`] keeps it true, and both are cleared
/// where a record is lost ([`gradient_db::unwalk_derivations`], the GC's orphan
/// reclaim). Build and cache state say nothing about whether the graph is recorded,
/// so keying on them re-walked a complete record for as long as its anchor had not
/// succeeded.
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

    /// A walked derivation above a stub is not prunable: its record is written,
    /// its subtree is not, and pruning there is exactly how an abandoned walk
    /// strands the stubs for every walk after it.
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
