/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Strip the `/nix/store/` prefix from `derivation.derivation_path` so the
//! column matches the narinfo `References:` convention already used by
//! `cached_path` (hash-name only, prefix added back at the worker / API
//! boundary).
//!
//! Storing the prefix in the DB lets it desync from the value the worker
//! actually sends in `NarRequest`: a stale row written by an older worker
//! without the prefix surfaced as `daemon ... invalid store path:
//! <hash>-<name>.drv` because the value flowed through the dispatcher and
//! WS unmodified. Stripping uniformly removes the ambiguity, and the read
//! path always reconstructs the canonical `/nix/store/<...>` form via
//! `gradient_core::executer::nix_store_path`.
//!
//! Conflict handling: a `(organization, derivation_path)` unique index
//! guards the table, so any pair of rows that already differ only by
//! prefix would collide on strip. The migration leaves such pairs alone
//! (NOT EXISTS guard) and lets the operator decide which row to keep;
//! the new read path's defensive `nix_store_path()` wrapper makes either
//! row safe to dispatch in the meantime.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{ConnectionTrait, Statement};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        let backend = db.get_database_backend();

        db.execute(Statement::from_string(
            backend,
            "UPDATE derivation \
             SET derivation_path = SUBSTRING(derivation_path FROM 12) \
             WHERE derivation_path LIKE '/nix/store/%' \
               AND NOT EXISTS ( \
                 SELECT 1 FROM derivation AS d2 \
                 WHERE d2.organization = derivation.organization \
                   AND d2.derivation_path = SUBSTRING(derivation.derivation_path FROM 12) \
               )",
        ))
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        let backend = db.get_database_backend();

        db.execute(Statement::from_string(
            backend,
            "UPDATE derivation \
             SET derivation_path = '/nix/store/' || derivation_path \
             WHERE derivation_path NOT LIKE '/nix/store/%' \
               AND NOT EXISTS ( \
                 SELECT 1 FROM derivation AS d2 \
                 WHERE d2.organization = derivation.organization \
                   AND d2.derivation_path = '/nix/store/' || derivation.derivation_path \
               )",
        ))
        .await?;
        Ok(())
    }
}
