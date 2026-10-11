/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::derivation_dependency::EdgeKind;
use gradient_types::*;
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter};

pub async fn of_derivations<C: ConnectionTrait>(
    db: &C,
    derivations: &[DerivationId],
) -> Result<Vec<MDerivationDependency>, DbErr> {
    crate::fetch_in_chunks(derivations, |chunk| {
        EDerivationDependency::find()
            .filter(CDerivationDependency::Derivation.is_in(chunk))
            .all(db)
    })
    .await
}

pub async fn build_inputs_of<C: ConnectionTrait>(
    db: &C,
    derivations: Vec<DerivationId>,
) -> Result<Vec<MDerivationDependency>, DbErr> {
    EDerivationDependency::find()
        .filter(CDerivationDependency::Derivation.is_in(derivations))
        .filter(CDerivationDependency::Kind.is_in([EdgeKind::Buildtime, EdgeKind::Both]))
        .all(db)
        .await
}
