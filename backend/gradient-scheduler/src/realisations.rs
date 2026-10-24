/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashMap;

use anyhow::{Context, Result};
use gradient_types::*;
use gradient_wire::types::Realisation;
use sea_orm::ConnectionTrait;

pub(crate) async fn realisations_by_derivation<C: ConnectionTrait>(
    db: &C,
    derivations: &[DerivationId],
) -> Result<HashMap<DerivationId, Vec<Realisation>>> {
    let realised = gradient_db::derivation_outputs::realised_after_build(db, derivations)
        .await
        .context("load the outputs realised by builds")?;
    let mut by_derivation: HashMap<DerivationId, Vec<Realisation>> = HashMap::new();
    for (derivation, output) in realised {
        let Some(path) = output.store_path() else {
            continue;
        };
        by_derivation
            .entry(derivation.id)
            .or_default()
            .push(Realisation {
                drv_path: derivation.store_path(),
                output: output.name,
                path: path.full(),
            });
    }

    Ok(by_derivation)
}
