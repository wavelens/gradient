/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::DerivationId;
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr, Value};

fn lock_shared_builds_sql(with_dependencies: bool) -> String {
    let (with, filter) = crate::graph::shared_build_guard::advisory_filter("$1", with_dependencies);
    format!(
        "WITH {with} SELECT 1 FROM derivation_build \
         WHERE derivation = ANY($1::uuid[]) AND {filter} \
         ORDER BY derivation FOR NO KEY UPDATE"
    )
}

fn lock_flip_shared_builds_sql() -> String {
    lock_shared_builds_sql(false)
}

fn lock_seed_shared_builds_sql() -> String {
    lock_shared_builds_sql(true)
}

crate::sql_fn! {
    LOCK_SHARED_BUILDS = lock_flip_shared_builds_sql,
        params = [DerivationIds(64)];

    LOCK_SEED_SHARED_BUILDS = lock_seed_shared_builds_sql,
        params = [DerivationIds(64)];
}

/// Proof that a batch of shared builds is held `FOR NO KEY UPDATE`, `derivation`-ordered, on `txn`.
/// Only [`lock_shared_builds`] constructs one, and [`seed_blocking_deps`](super::seed_blocking_deps),
/// [`became_fetchable`], [`lost_fetchability`] and the two recounts accept nothing
/// else, so none of them can run unlocked, on a pooled handle where the lock is
/// released at the end of the statement that took it, in another transaction, or over
/// a row the lock did not name.
///
/// It also makes a flip ATOMIC, which is the property the counter actually needs: the
/// mark and its compensating ripple land or roll back together, so a killed ripple
/// leaves nothing to retry around. What the proof does NOT cover is the ripple's own
/// write set, the flipped shared builds' parents, which no ordered lock names; the module
/// doc says why that is sound and what it costs. The caveat on
/// The caveat on every lock proof applies here too: it says the write
/// follows the lock, not that no read preceded it.
#[must_use = "a lock proves nothing unless a write runs on it"]
pub struct SharedBuildLock<'txn> {
    pub(super) txn: &'txn DatabaseTransaction,
    pub(super) derivations: Vec<DerivationId>,
}

/// Take `derivations` `FOR NO KEY UPDATE` in one `derivation`-ordered statement, before the
/// caller decides anything, with each shared build's advisory key held exclusively ahead of
/// its row (see [`crate::graph::shared_build_guard`]). With acquisition monotone in `derivation` a
/// wait-for cycle would need some transaction to wait on a lower id than one it
/// already holds; the ripples acquire in plan order and are outside that, which the
/// module doc accounts for. An empty batch locks nothing and issues no statement.
pub async fn lock_shared_builds<'txn>(
    txn: &'txn DatabaseTransaction,
    derivations: &[DerivationId],
) -> Result<SharedBuildLock<'txn>, DbErr> {
    if !derivations.is_empty() {
        txn.execute_raw(LOCK_SHARED_BUILDS.bind([ids(derivations)]))
            .await?;
    }

    Ok(SharedBuildLock {
        txn,
        derivations: derivations.to_vec(),
    })
}

/// [`SharedBuildLock`] for a seed: the shared builds' keys exclusively and their dependencies'
/// keys shared, held to commit, so the count a seed writes cannot miss a flip of what
/// it counts. The only proof [`seed_blocking_deps`](super::seed_blocking_deps) accepts.
#[must_use = "a lock proves nothing unless a write runs on it"]
pub struct SeedLock<'txn>(SharedBuildLock<'txn>);

impl<'txn> std::ops::Deref for SeedLock<'txn> {
    type Target = SharedBuildLock<'txn>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

pub async fn lock_seed_shared_builds<'txn>(
    txn: &'txn DatabaseTransaction,
    derivations: &[DerivationId],
) -> Result<SeedLock<'txn>, DbErr> {
    if !derivations.is_empty() {
        txn.execute_raw(LOCK_SEED_SHARED_BUILDS.bind([ids(derivations)]))
            .await?;
    }

    Ok(SeedLock(SharedBuildLock {
        txn,
        derivations: derivations.to_vec(),
    }))
}

pub(crate) fn ids(derivations: &[DerivationId]) -> Value {
    derivations
        .iter()
        .map(|d| d.into_inner())
        .collect::<Vec<uuid::Uuid>>()
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shared_build_lock_takes_its_advisory_keys_before_the_row_locks() {
        let sql = LOCK_SHARED_BUILDS.text();
        assert!(
            sql.starts_with("WITH shared_build_keys AS MATERIALIZED ("),
            "{sql}"
        );
        assert!(
            sql.contains(
                "(SELECT n FROM shared_build_locks) >= 0 ORDER BY derivation FOR NO KEY UPDATE"
            ),
            "{sql}"
        );
        assert!(!sql.contains("derivation_dependency"), "{sql}");

        let seed = LOCK_SEED_SHARED_BUILDS.text();
        assert!(
            seed.contains("pg_advisory_xact_lock_shared(643, k)"),
            "{seed}"
        );
        assert!(
            seed.contains("ORDER BY derivation FOR NO KEY UPDATE"),
            "{seed}"
        );
    }
}
