/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::DbContext;
use anyhow::{Context, Result};
use gradient_types::*;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter};
use sea_orm_migration::prelude::*;

pub async fn add_features(
    ctx: &DbContext,
    features: Vec<String>,
    kind: gradient_entity::feature::FeatureKind,
    derivation_id: Option<DerivationId>,
) -> Result<()> {
    for f in features {
        let feature = EFeature::find()
            .filter(CFeature::Name.eq(f.clone()))
            .filter(CFeature::Kind.eq(kind.clone()))
            .one(&ctx.worker_db)
            .await
            .context("Failed to query feature")?;

        let feature = if let Some(f) = feature {
            f
        } else {
            let afeature = MFeature {
                id: FeatureId::now_v7(),
                name: f,
                kind: kind.clone(),
            }
            .into_active_model();

            afeature
                .insert(&ctx.worker_db)
                .await
                .context("Failed to insert feature")?
        };

        if let Some(d_id) = derivation_id {
            let aderivation_feature = MDerivationFeature {
                id: DerivationFeatureId::now_v7(),
                derivation: d_id,
                feature: feature.id,
            }
            .into_active_model();

            // `derivation_feature` has a UNIQUE (derivation, feature) index;
            // re-discovering an already-known edge during a fresh evaluation
            // would otherwise blow up with a constraint violation and abort
            // the whole eval-result handler. `ON CONFLICT DO NOTHING` makes
            // the insert idempotent.
            EDerivationFeature::insert(aderivation_feature)
                .on_conflict(
                    sea_orm::sea_query::OnConflict::columns([
                        CDerivationFeature::Derivation,
                        CDerivationFeature::Feature,
                    ])
                    .do_nothing()
                    .to_owned(),
                )
                .try_insert()
                .exec(&ctx.worker_db)
                .await
                .context("Failed to insert derivation feature")?;
        }
    }
    Ok(())
}
