/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! A reference recorded while its producer was still an unwalked stub resolved to
//! no derivation, so its runtime edge was never written and the referrer read whole
//! over a path nobody built. The walk now adopts such references through these
//! indexes; the backfill writes the edges history lost, and the consistency sweep
//! recounts the wholeness they change.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &[&str] = &[
    r#"CREATE INDEX IF NOT EXISTS "idx-cached_path-references"
     ON cached_path USING gin (string_to_array("references", ' '))"#,
    r#"CREATE INDEX IF NOT EXISTS "idx-derivation_output-references_list"
     ON derivation_output USING gin (string_to_array(references_list, ' '))"#,
    r#"INSERT INTO derivation_dependency (derivation, dependency, kind)
     SELECT DISTINCT r.derivation, o.derivation, 1
     FROM (SELECT cp.hash AS referrer, t.token FROM cached_path cp,
                  unnest(string_to_array(cp."references", ' ')) AS t(token)
           UNION ALL
           SELECT ro.hash, t.token FROM derivation_output ro,
                  unnest(string_to_array(ro.references_list, ' ')) AS t(token)) refs
     JOIN derivation_output r ON r.hash = refs.referrer
     JOIN derivation_output o
       ON o.hash = split_part(regexp_replace(refs.token, '^/nix/store/', ''), '-', 1)
     WHERE r.derivation <> o.derivation
     ON CONFLICT (derivation, dependency) DO UPDATE SET kind = 2
     WHERE derivation_dependency.kind = 0"#,
];

const DOWN: &[&str] = &[
    r#"DROP INDEX IF EXISTS "idx-derivation_output-references_list""#,
    r#"DROP INDEX IF EXISTS "idx-cached_path-references""#,
];

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for stmt in UP {
            manager.get_connection().execute_unprepared(stmt).await?;
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for stmt in DOWN {
            manager.get_connection().execute_unprepared(stmt).await?;
        }

        Ok(())
    }
}
