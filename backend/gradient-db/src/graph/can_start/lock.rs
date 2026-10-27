/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::DerivationId;
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr, RuntimeErr, Value};

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

fn try_lock_shared_builds_sql() -> String {
    let (with, filter) = crate::graph::shared_build_guard::try_advisory_filter("$1");
    format!(
        "WITH {with} SELECT 1 FROM derivation_build \
         WHERE derivation = ANY($1::uuid[]) AND {filter} \
         ORDER BY derivation FOR NO KEY UPDATE NOWAIT"
    )
}

crate::sql_fn! {
    LOCK_SHARED_BUILDS = lock_flip_shared_builds_sql,
        params = [DerivationIds(64)];

    LOCK_SEED_SHARED_BUILDS = lock_seed_shared_builds_sql,
        params = [DerivationIds(64)];

    TRY_LOCK_SHARED_BUILDS = try_lock_shared_builds_sql,
        params = [DerivationIds(64)];
}

/// Only [`lock_shared_builds`] is constructing this proof.
/// The flip writers are accepting nothing else, which is making a flip atomic.
/// Its mark and its ripple are landing or rolling back together.
#[must_use = "a lock proves nothing unless a write runs on it"]
pub struct SharedBuildLock<'txn> {
    pub(super) txn: &'txn DatabaseTransaction,
    pub(super) derivations: Vec<DerivationId>,
}

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

pub async fn try_lock_shared_builds<'txn>(
    txn: &'txn DatabaseTransaction,
    derivations: &[DerivationId],
) -> Result<Option<SharedBuildLock<'txn>>, DbErr> {
    if derivations.is_empty() {
        return Ok(Some(SharedBuildLock {
            txn,
            derivations: Vec::new(),
        }));
    }

    match txn
        .query_all_raw(TRY_LOCK_SHARED_BUILDS.bind([ids(derivations)]))
        .await
    {
        Ok(rows) if rows.is_empty() => Ok(None),
        Ok(_) => Ok(Some(SharedBuildLock {
            txn,
            derivations: derivations.to_vec(),
        })),
        Err(e) if lock_unavailable(&e) => Ok(None),
        Err(e) => Err(e),
    }
}

fn lock_unavailable(err: &DbErr) -> bool {
    let (DbErr::Query(RuntimeErr::SqlxError(e)) | DbErr::Exec(RuntimeErr::SqlxError(e))) = err
    else {
        return false;
    };

    e.as_database_error()
        .and_then(|d| d.code())
        .is_some_and(|code| code == "55P03")
}

/// Dependencies' keys are held shared until commit.
/// A seed count can then never miss a flip of what it is counting.
#[must_use = "a lock proves nothing unless a write runs on it"]
pub struct SeedLock<'txn>(SharedBuildLock<'txn>);

impl<'txn> std::ops::Deref for SeedLock<'txn> {
    type Target = SharedBuildLock<'txn>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[tracing::instrument(level = "debug", skip_all)]
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

        let try_lock = TRY_LOCK_SHARED_BUILDS.text();
        assert!(
            try_lock.contains("pg_try_advisory_xact_lock(643, k)")
                && try_lock.ends_with("ORDER BY derivation FOR NO KEY UPDATE NOWAIT"),
            "{try_lock}"
        );
    }
}
