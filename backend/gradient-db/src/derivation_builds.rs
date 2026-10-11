/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::*;
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter};

pub async fn of_derivation<C: ConnectionTrait>(
    db: &C,
    derivation: DerivationId,
) -> Result<Option<MDerivationBuild>, DbErr> {
    EDerivationBuild::find()
        .filter(CDerivationBuild::Derivation.eq(derivation))
        .one(db)
        .await
}

pub async fn of_derivations<C: ConnectionTrait>(
    db: &C,
    derivations: &[DerivationId],
) -> Result<Vec<MDerivationBuild>, DbErr> {
    EDerivationBuild::find()
        .filter(CDerivationBuild::Derivation.is_in(derivations.iter().copied()))
        .all(db)
        .await
}

pub async fn by_derivation<C: ConnectionTrait>(
    db: &C,
    derivation: DerivationId,
) -> Result<Option<MDerivationBuild>, DbErr> {
    EDerivationBuild::find()
        .filter(CDerivationBuild::Derivation.eq(derivation))
        .one(db)
        .await
}
