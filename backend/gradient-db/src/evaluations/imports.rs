/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::build::BuildStatus;
use gradient_types::*;
use sea_orm::{ConnectionTrait, DbErr, FromQueryResult};

crate::sql! {
    IMPORT_BUILDS = r#"
SELECT bj.id AS build_id, bj.derivation_build, db.status, d.hash
FROM derivation d
JOIN build_job bj ON bj.derivation = d.id AND bj.evaluation = $1
JOIN derivation_build db ON db.id = bj.derivation_build
WHERE d.hash = ANY($2::text[])
"#,
        params = [EvaluationId, DerivationHashes(64)];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportBuild {
    pub build_id: BuildJobId,
    pub derivation_build: DerivationBuildId,
    pub status: BuildStatus,
}

#[derive(FromQueryResult)]
struct ImportBuildRow {
    build_id: uuid::Uuid,
    derivation_build: uuid::Uuid,
    status: i32,
    hash: String,
}

pub async fn import_builds<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
    hashes: Vec<String>,
) -> Result<std::collections::HashMap<String, ImportBuild>, DbErr> {
    let rows = ImportBuildRow::find_by_statement(
        IMPORT_BUILDS.bind([evaluation.into_inner().into(), hashes.into()]),
    )
    .all(db)
    .await?;

    rows.into_iter()
        .map(|r| {
            let status = BuildStatus::try_from(r.status)
                .map_err(|e| DbErr::Custom(format!("derivation_build status: {e}")))?;
            Ok((
                r.hash,
                ImportBuild {
                    build_id: BuildJobId::from(r.build_id),
                    derivation_build: DerivationBuildId::from(r.derivation_build),
                    status,
                },
            ))
        })
        .collect()
}
