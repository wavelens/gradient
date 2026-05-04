/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Convert `cached_path_signature.signature` from `text` (base64-encoded
//! signature, sometimes prefixed with `keyname:`) to `bytea` storing the
//! raw 64-byte Ed25519 signature.
//!
//! Storage win: ~88 bytes (base64 + padding) → 64 bytes per row, plus the
//! varlena overhead difference. The keyname is reconstructed at read time
//! from `cache.name` + `serve_url` so it does not need to live in the row.
//!
//! Existing rows may hold either `keyname:base64` (recent writes) or bare
//! `base64` (legacy writes). The `USING` clause strips an optional `keyname:`
//! prefix before base64-decoding.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        conn.execute_unprepared(
            r#"
            ALTER TABLE cached_path_signature
            ALTER COLUMN signature TYPE bytea
            USING decode(
                CASE
                    WHEN signature IS NULL THEN NULL
                    WHEN position(':' in signature) > 0
                        THEN substring(signature from position(':' in signature) + 1)
                    ELSE signature
                END,
                'base64'
            )
            "#,
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        conn.execute_unprepared(
            r#"
            ALTER TABLE cached_path_signature
            ALTER COLUMN signature TYPE text
            USING encode(signature, 'base64')
            "#,
        )
        .await?;
        Ok(())
    }
}
