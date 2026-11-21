/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::*;
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter};
use std::collections::{HashMap, HashSet};

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

pub async fn realised_after_build<C: ConnectionTrait>(
    db: &C,
    derivations: &[DerivationId],
) -> Result<Vec<(MDerivation, MDerivationOutput)>, DbErr> {
    let outputs = crate::fetch_in_chunks(derivations, |chunk| async move {
        EDerivationOutput::find()
            .filter(CDerivationOutput::Derivation.is_in(chunk))
            .filter(CDerivationOutput::PathKnownAfterBuild.eq(true))
            .filter(CDerivationOutput::Hash.is_not_null())
            .all(db)
            .await
    })
    .await?;
    let producers: Vec<DerivationId> = outputs.iter().map(|o| o.derivation).collect();
    let by_id: HashMap<DerivationId, MDerivation> =
        crate::fetch_in_chunks(&producers, |chunk| async move {
            EDerivation::find()
                .filter(CDerivation::Id.is_in(chunk))
                .all(db)
                .await
        })
        .await?
        .into_iter()
        .map(|d| (d.id, d))
        .collect();

    Ok(outputs
        .into_iter()
        .filter_map(|o| Some((by_id.get(&o.derivation)?.clone(), o)))
        .collect())
}

pub async fn realised_output<C: ConnectionTrait>(
    db: &C,
    drv_hash: &str,
    output: &str,
) -> Result<Option<MDerivationOutput>, DbErr> {
    let Some(derivation) = EDerivation::find()
        .filter(CDerivation::Hash.eq(drv_hash))
        .one(db)
        .await?
    else {
        return Ok(None);
    };

    EDerivationOutput::find()
        .filter(CDerivationOutput::Derivation.eq(derivation.id))
        .filter(CDerivationOutput::Name.eq(output))
        .filter(CDerivationOutput::PathKnownAfterBuild.eq(true))
        .filter(CDerivationOutput::Hash.is_not_null())
        .one(db)
        .await
}

pub async fn output_ids_skipping_build_or_probe_for_external_url_or_cached_variant_trusted_by_shared_build<
    C: ConnectionTrait,
>(
    db: &C,
    outputs: &[MDerivationOutput],
) -> Result<HashSet<DerivationOutputId>, DbErr> {
    let wanted: Vec<(DerivationId, String)> = outputs
        .iter()
        .filter_map(|o| Some((o.derivation, o.hash.clone()?)))
        .collect();
    let stored = crate::store_path_content_variant_reader_choice::highest_ranked_trusted_content_variant_by_derivation_id_and_store_path_hash_for_shared_build_own_outputs(db, &wanted).await?;
    Ok(outputs
        .iter()
        .filter(|o| {
            o.external_url.is_some()
                || o.hash
                    .as_ref()
                    .is_some_and(|h| stored.contains_key(&(o.derivation, h.clone())))
        })
        .map(|o| o.id)
        .collect())
}
