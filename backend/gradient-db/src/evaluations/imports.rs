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

fn pending_imports_sql() -> String {
    format!(
        "SELECT db.* FROM derivation d \
         JOIN build_job bj ON bj.derivation = d.id \
         JOIN derivation_build db ON db.id = bj.derivation_build \
         WHERE d.ifd AND bj.evaluation = ANY($1::uuid[]) AND db.status IN ({})",
        crate::sql::status::build_in(&BuildStatus::PENDING)
    )
}

crate::sql_fn! {
    PENDING_IMPORTS = pending_imports_sql,
        params = [EvaluationIds(64)];
}

pub async fn pending_imports<C: ConnectionTrait>(
    db: &C,
    evaluations: &[EvaluationId],
) -> Result<Vec<MDerivationBuild>, DbErr> {
    use sea_orm::EntityTrait;

    if evaluations.is_empty() {
        return Ok(Vec::new());
    }

    let ids: Vec<uuid::Uuid> = evaluations.iter().map(|e| e.into_inner()).collect();
    EDerivationBuild::find()
        .from_raw_sql(PENDING_IMPORTS.bind([ids.into()]))
        .all(db)
        .await
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
