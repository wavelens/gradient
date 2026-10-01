/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::status::TransitionChange;

use gradient_entity::build::BuildStatus;
use gradient_types::DerivationId;
use sea_orm::QueryResult;

/// Collect the `derivation` column of a `RETURNING derivation` result set. The
/// bulk transitions return the shared builds they actually moved so the caller can fan
/// the CI status reactor out over exactly those (and only those) builds.
pub(crate) fn returned_derivations(rows: Vec<QueryResult>) -> Vec<DerivationId> {
    rows.into_iter()
        .filter_map(|r| r.try_get::<uuid::Uuid>("", "derivation").ok())
        .map(DerivationId::new)
        .collect()
}

/// Collect `RETURNING db.derivation, old.status AS from_status, db.status AS
/// to_status` rows into the typed changes the effects emitter consumes. `old` is
/// Postgres 18's pre-update row, which a self-join used to fetch at the price of
/// a sequential scan once a statement moved many shared builds.
pub(crate) fn returned_transitions(rows: Vec<QueryResult>) -> Vec<TransitionChange> {
    rows.into_iter()
        .filter_map(|r| {
            let derivation = r.try_get::<uuid::Uuid>("", "derivation").ok()?;
            let from = BuildStatus::try_from(r.try_get::<i32>("", "from_status").ok()?).ok()?;
            let to = BuildStatus::try_from(r.try_get::<i32>("", "to_status").ok()?).ok()?;
            Some(TransitionChange {
                derivation: DerivationId::new(derivation),
                from,
                to,
            })
        })
        .collect()
}

/// Changes for rows a statement moved from a statically-known status (e.g. a
/// `WHERE status = Created` promote): no self-join needed, the predicate is the proof.
pub(crate) fn transitions_from(
    derivations: Vec<DerivationId>,
    from: BuildStatus,
    to: BuildStatus,
) -> Vec<TransitionChange> {
    derivations
        .into_iter()
        .map(|derivation| TransitionChange {
            derivation,
            from,
            to,
        })
        .collect()
}
