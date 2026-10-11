/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::*;
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QuerySelect};

pub async fn with_ids<C: ConnectionTrait>(
    db: &C,
    ids: &[DerivationId],
) -> Result<Vec<MDerivation>, DbErr> {
    crate::fetch_in_chunks(ids, |chunk| {
        EDerivation::find()
            .filter(CDerivation::Id.is_in(chunk))
            .all(db)
    })
    .await
}

pub async fn names<C: ConnectionTrait>(
    db: &C,
    derivations: &[DerivationId],
) -> Result<Vec<(DerivationId, String)>, DbErr> {
    crate::fetch_in_chunks(derivations, |chunk| async move {
        EDerivation::find()
            .select_only()
            .column(CDerivation::Id)
            .column(CDerivation::Name)
            .filter(CDerivation::Id.is_in(chunk))
            .into_tuple::<(DerivationId, String)>()
            .all(db)
            .await
    })
    .await
}
