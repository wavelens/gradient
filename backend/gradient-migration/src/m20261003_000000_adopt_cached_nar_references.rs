/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &str = r#"INSERT INTO derivation_dependency (derivation, dependency, kind)
 SELECT DISTINCT o.derivation, p.derivation, 1
 FROM derivation_output o
 JOIN cached_path cp ON cp.hash = o.hash
 CROSS JOIN LATERAL (SELECT array_agg(tw.derivation) AS twins
                     FROM derivation_output tw WHERE tw.hash = o.hash) tw
 CROSS JOIN LATERAL unnest(string_to_array(cp."references", ' ')) AS t(token)
 JOIN derivation_output p
   ON p.hash = split_part(regexp_replace(t.token, '^/nix/store/', ''), '-', 1)
  AND p.derivation <> ALL(tw.twins)
 ON CONFLICT (derivation, dependency) DO UPDATE SET kind = 2
 WHERE derivation_dependency.kind = 0"#;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP).await?;

        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}
