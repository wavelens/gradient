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

crate::sql! {
    BUILD_INPUT_PNAMES = r#"
SELECT coalesce(nullif(d.pname, ''), regexp_replace(d.name, '-[0-9][^-]*$', '')) COLLATE "C" AS pname
FROM derivation_dependency e JOIN derivation d ON d.id = e.dependency
WHERE e.derivation = $1 AND e.kind IN (0, 2)
GROUP BY 1 ORDER BY 1
"#,
        params = [DerivationId];
}

pub async fn build_input_pnames<C: ConnectionTrait>(
    db: &C,
    derivation: DerivationId,
) -> Result<Vec<String>, DbErr> {
    let rows = db
        .query_all_raw(
            BUILD_INPUT_PNAMES.bind([sea_orm::Value::Uuid(Some(derivation.into_inner()))]),
        )
        .await?;
    Ok(rows
        .iter()
        .filter_map(|r| r.try_get::<String>("", "pname").ok())
        .collect())
}

crate::sql! {
    FEWEST_OUT_OF_MEMORY_CORES = r#"
SELECT min(build_cores) AS cores
FROM derivation_metric
WHERE derivation = $1 AND oom_killed AND build_cores IS NOT NULL
"#,
        params = [DerivationId];
}

pub async fn fewest_out_of_memory_cores<C: ConnectionTrait>(
    db: &C,
    derivation: DerivationId,
) -> Result<Option<u32>, DbErr> {
    let row = db
        .query_one_raw(
            FEWEST_OUT_OF_MEMORY_CORES.bind([sea_orm::Value::Uuid(Some(derivation.into_inner()))]),
        )
        .await?;
    Ok(row
        .and_then(|r| r.try_get::<Option<i32>>("", "cores").ok().flatten())
        .and_then(|c| u32::try_from(c).ok()))
}
