/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::*;
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter};

pub async fn of_outputs<C: ConnectionTrait>(
    db: &C,
    outputs: &[DerivationOutputId],
) -> Result<Vec<MBuildProduct>, DbErr> {
    crate::fetch_in_chunks(outputs, |chunk| async move {
        EBuildProduct::find()
            .filter(CBuildProduct::DerivationOutput.is_in(chunk))
            .all(db)
            .await
    })
    .await
}
