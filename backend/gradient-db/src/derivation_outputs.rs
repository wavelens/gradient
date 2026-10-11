/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::*;
use sea_orm::{ColumnTrait, Condition, ConnectionTrait, DbErr, EntityTrait, QueryFilter};

pub async fn cached_of_derivations<C: ConnectionTrait>(
    db: &C,
    derivations: Vec<DerivationId>,
) -> Result<Vec<MDerivationOutput>, DbErr> {
    EDerivationOutput::find()
        .filter(CDerivationOutput::Derivation.is_in(derivations))
        .filter(CDerivationOutput::CachedPath.is_not_null())
        .all(db)
        .await
}

pub async fn of_derivation<C: ConnectionTrait>(
    db: &C,
    derivation: DerivationId,
) -> Result<Vec<MDerivationOutput>, DbErr> {
    EDerivationOutput::find()
        .filter(CDerivationOutput::Derivation.eq(derivation))
        .all(db)
        .await
}

pub async fn of_derivations<C: ConnectionTrait>(
    db: &C,
    derivations: &[DerivationId],
) -> Result<Vec<MDerivationOutput>, DbErr> {
    crate::fetch_in_chunks(derivations, |chunk| async move {
        EDerivationOutput::find()
            .filter(CDerivationOutput::Derivation.is_in(chunk))
            .all(db)
            .await
    })
    .await
}

pub async fn cached_by_hash<C: ConnectionTrait>(
    db: &C,
    hash: &str,
) -> Result<Option<MDerivationOutput>, DbErr> {
    EDerivationOutput::find()
        .filter(
            Condition::all()
                .add(CDerivationOutput::IsCached.eq(true))
                .add(CDerivationOutput::Hash.eq(hash)),
        )
        .one(db)
        .await
}
