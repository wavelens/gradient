/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Seed the inbound + outbound GitHub App integration rows for every org that
//! already has a `github_installation_id`.
//!
//! GitHub forge support is delivered through the server-wide GitHub App, so
//! the `integration` rows that represent it carry no per-row credentials —
//! they are stable handles the trigger lookup (`resolve_github_integration_id`)
//! and the project_integration outbound link can reference. The runtime hook
//! that records new App installations creates these rows going forward;
//! this migration backfills them for orgs that bound an installation before
//! that hook was wired in.
//!
//! Project-level outbound choices are intentionally NOT backfilled: any
//! project that previously got status reporting through the URL-based
//! auto-detection must explicitly opt in via the new dropdown.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        // kind: 0 = inbound, 1 = outbound. forge_type: 3 = github.
        for kind in [0_i16, 1_i16] {
            db.execute_unprepared(&format!(
                r#"
                INSERT INTO integration
                    (id, organization, name, display_name, kind, forge_type,
                     secret, endpoint_url, access_token, created_by, created_at)
                SELECT
                    gen_random_uuid(), o.id, 'github', 'GitHub',
                    {kind}, 3, NULL, NULL, NULL, o.created_by, NOW()
                FROM organization o
                WHERE o.github_installation_id IS NOT NULL
                  AND NOT EXISTS (
                      SELECT 1 FROM integration i
                      WHERE i.organization = o.id
                        AND i.kind = {kind}
                        AND i.forge_type = 3
                  )
                ON CONFLICT DO NOTHING
                "#,
            ))
            .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "DELETE FROM integration \
                 WHERE forge_type = 3 \
                   AND name = 'github' \
                   AND secret IS NULL \
                   AND endpoint_url IS NULL \
                   AND access_token IS NULL",
            )
            .await?;
        Ok(())
    }
}
