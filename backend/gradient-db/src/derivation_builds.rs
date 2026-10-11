/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::*;
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QuerySelect};

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

pub async fn derivations_of<C: ConnectionTrait>(
    db: &C,
    derivation_builds: &[DerivationBuildId],
) -> Result<Vec<(DerivationId, DerivationBuildId)>, DbErr> {
    crate::fetch_in_chunks(derivation_builds, |chunk| async move {
        EDerivationBuild::find()
            .select_only()
            .column(CDerivationBuild::Derivation)
            .column(CDerivationBuild::Id)
            .filter(CDerivationBuild::Id.is_in(chunk))
            .into_tuple()
            .all(db)
            .await
    })
    .await
}

pub async fn first_with_status_among<C: ConnectionTrait>(
    db: &C,
    derivations: Vec<DerivationId>,
    statuses: Vec<gradient_entity::build::BuildStatus>,
) -> Result<Option<MDerivationBuild>, DbErr> {
    EDerivationBuild::find()
        .filter(CDerivationBuild::Derivation.is_in(derivations))
        .filter(CDerivationBuild::Status.is_in(statuses))
        .one(db)
        .await
}
