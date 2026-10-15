/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::*;
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter};
use std::collections::HashSet;

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

pub async fn cached_anywhere<C: ConnectionTrait>(
    db: &C,
    outputs: &[MDerivationOutput],
) -> Result<HashSet<DerivationOutputId>, DbErr> {
    let hashes: Vec<String> = outputs.iter().map(|o| o.hash.clone()).collect();
    let stored = crate::cached_paths::stored_by_hash(db, &hashes).await?;
    Ok(outputs
        .iter()
        .filter(|o| o.external_url.is_some() || stored.contains_key(&o.hash))
        .map(|o| o.id)
        .collect())
}
