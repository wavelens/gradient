/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::*;
use sea_orm::{ConnectionTrait, DbErr, Value};
use std::collections::HashSet;

crate::sql! {
    PRODUCERS_OF_HASHES = "SELECT DISTINCT o.derivation FROM derivation_output o WHERE o.hash = ANY($1)",
        params = [CachedPathHashes(64)];

    INHERIT_NAMES = "INSERT INTO build_job \
         (id, evaluation, derivation, derivation_build, score, score_breakdown, created_at) \
         SELECT uuidv7(), $2, bj.derivation, bj.derivation_build, 0, '{}'::jsonb, \
         (now() AT TIME ZONE 'UTC') \
         FROM build_job bj WHERE bj.evaluation = $1 \
         ON CONFLICT (evaluation, derivation) DO NOTHING",
        params = [EvaluationId, EvaluationId],
        tier = Bulk,
        budget = crate::sql::Budget::bulk().buffers(650_000)
            .because("copies every name of the evaluation, ~5 buffers per row across the \
                      heap and its indexes, and the fixture's largest names ~98k");
}

pub async fn inherit_names<C: ConnectionTrait>(
    db: &C,
    from: EvaluationId,
    to: EvaluationId,
) -> Result<u64, DbErr> {
    Ok(db
        .execute_raw(INHERIT_NAMES.bind([
            Value::Uuid(Some(from.into_inner())),
            Value::Uuid(Some(to.into_inner())),
        ]))
        .await?
        .rows_affected())
}

pub async fn producers_of_hashes<C: ConnectionTrait>(
    db: &C,
    hashes: &[String],
) -> Result<Vec<DerivationId>, DbErr> {
    if hashes.is_empty() {
        return Ok(Vec::new());
    }

    let rows = db
        .query_all_raw(PRODUCERS_OF_HASHES.bind([hashes.to_vec().into()]))
        .await?;

    Ok(rows
        .iter()
        .filter_map(|r| r.try_get::<uuid::Uuid>("", "derivation").ok())
        .map(DerivationId::new)
        .collect())
}

crate::sql! {
    /// The reserved `build-request` task is always signable, whatever its `sign_cache` flag.
    /// The submitting client must substitute its outputs.
    PRIVATE_OUTPUT_HASHES = "SELECT DISTINCT o.hash FROM derivation_output o \
             WHERE o.hash = ANY($1) \
               AND EXISTS (SELECT 1 FROM build_job b WHERE b.derivation = o.derivation) \
               AND NOT EXISTS (SELECT 1 FROM derivation_output s \
                   JOIN build_job b ON b.derivation = s.derivation \
                   JOIN evaluation e ON e.id = b.evaluation \
                   JOIN task t ON t.id = e.task \
                   WHERE s.hash = o.hash AND (t.sign_cache OR t.name = 'build-request'))",
        params = [CachedPathHashes(64)];
}

pub async fn private_output_hashes<C: ConnectionTrait>(
    db: &C,
    hashes: &[String],
) -> Result<HashSet<String>, DbErr> {
    if hashes.is_empty() {
        return Ok(HashSet::new());
    }

    let rows = db
        .query_all_raw(PRIVATE_OUTPUT_HASHES.bind([hashes.to_vec().into()]))
        .await?;

    Ok(rows
        .iter()
        .filter_map(|r| r.try_get::<String>("", "hash").ok())
        .collect())
}

crate::sql! {
    DERIVATIONS_WITH_HASHES = "SELECT d.id FROM derivation d WHERE d.hash = ANY($1)",
        params = [DerivationHashes(64)];
}

pub async fn derivations_with_hashes<C: ConnectionTrait>(
    db: &C,
    hashes: &[String],
) -> Result<Vec<DerivationId>, DbErr> {
    if hashes.is_empty() {
        return Ok(Vec::new());
    }

    let rows = db
        .query_all_raw(DERIVATIONS_WITH_HASHES.bind([hashes.to_vec().into()]))
        .await?;

    Ok(rows
        .iter()
        .filter_map(|r| r.try_get::<uuid::Uuid>("", "id").ok())
        .map(DerivationId::new)
        .collect())
}
