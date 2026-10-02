/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use sea_orm::sea_query::OnConflict;
use sea_orm::{ConnectionTrait, EntityTrait, Set};

use gradient_types::*;

pub async fn all<C: ConnectionTrait>(conn: &C) -> Result<Vec<MStorageMigration>> {
    EStorageMigration::find()
        .all(conn)
        .await
        .context("list storage_migration")
}

pub async fn save_checkpoint<C: ConnectionTrait>(
    conn: &C,
    name: &str,
    checkpoint: &str,
) -> Result<()> {
    EStorageMigration::insert(AStorageMigration {
        name: Set(name.to_owned()),
        checkpoint: Set(Some(checkpoint.to_owned())),
        started_at: Set(now()),
        applied_at: Set(None),
    })
    .on_conflict(
        OnConflict::column(CStorageMigration::Name)
            .update_column(CStorageMigration::Checkpoint)
            .to_owned(),
    )
    .exec_without_returning(conn)
    .await
    .context("save storage_migration checkpoint")?;
    Ok(())
}

pub async fn mark_applied<C: ConnectionTrait>(conn: &C, name: &str) -> Result<()> {
    EStorageMigration::insert(AStorageMigration {
        name: Set(name.to_owned()),
        checkpoint: Set(None),
        started_at: Set(now()),
        applied_at: Set(Some(now())),
    })
    .on_conflict(
        OnConflict::column(CStorageMigration::Name)
            .update_columns([CStorageMigration::Checkpoint, CStorageMigration::AppliedAt])
            .to_owned(),
    )
    .exec_without_returning(conn)
    .await
    .context("mark storage_migration applied")?;
    Ok(())
}
