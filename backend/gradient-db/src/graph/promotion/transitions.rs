/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::status::TransitionChange;

use gradient_entity::build::BuildStatus;
use gradient_types::DerivationId;
use sea_orm::QueryResult;

pub(crate) fn returned_derivations(rows: Vec<QueryResult>) -> Vec<DerivationId> {
    rows.into_iter()
        .filter_map(|r| r.try_get::<uuid::Uuid>("", "derivation").ok())
        .map(DerivationId::new)
        .collect()
}

/// `old` is the Postgres 18 pre-update row.
/// A self-join used to fetch it, at the price of a sequential scan.
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
