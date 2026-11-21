/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{ConnectionTrait, Statement};

#[derive(DeriveMigrationName)]
pub struct Migration;

/// A frozen copy of the ValidatedHttpBaseUrl rules at the time of this migration. Metrics key on
/// this exact string. Stored URLs that are different from it don't match those metrics.
fn normalise_upstream_cache_base_url(raw: &str) -> Option<String> {
    let parsed = url::Url::parse(raw.trim()).ok()?;
    let valid = matches!(parsed.scheme(), "http" | "https")
        && parsed.host_str().is_some_and(|host| !host.is_empty())
        && parsed.username().is_empty()
        && parsed.password().is_none()
        && parsed.query().is_none()
        && parsed.fragment().is_none();
    valid.then(|| parsed.as_str().trim_end_matches('/').to_owned())
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        let backend = db.get_database_backend();
        db.execute_unprepared("SET LOCAL lock_timeout = '5s';")
            .await?;
        let rows = db
            .query_all_raw(Statement::from_string(
                backend,
                "SELECT id, url FROM public.cache_upstream WHERE url IS NOT NULL",
            ))
            .await?;
        for row in rows {
            let id: uuid::Uuid = row.try_get("", "id")?;
            let stored: String = row.try_get("", "url")?;
            let Some(normalised) = normalise_upstream_cache_base_url(&stored) else {
                continue;
            };
            if normalised != stored {
                db.execute_raw(Statement::from_sql_and_values(
                    backend,
                    "UPDATE public.cache_upstream SET url = $1 WHERE id = $2",
                    [normalised.into(), id.into()],
                ))
                .await?;
            }
        }
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}
