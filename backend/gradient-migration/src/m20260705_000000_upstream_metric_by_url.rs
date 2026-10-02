/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UPSTREAM_METRICS: &str =
    "'upstream.latency_ms','upstream.narinfo_hits','upstream.narinfo_misses'";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared("ALTER TABLE upstream_metric ADD COLUMN upstream_url text")
            .await?;
        db.execute_unprepared(
            "UPDATE upstream_metric um SET upstream_url = cu.url \
             FROM cache_upstream cu WHERE cu.id = um.upstream",
        )
        .await?;

        db.execute_unprepared("ALTER TABLE upstream_metric DROP COLUMN upstream")
            .await?;
        db.execute_unprepared("DELETE FROM upstream_metric WHERE upstream_url IS NULL")
            .await?;

        // Every CTE is reading the pre-delete snapshot. `merged` is summing the original rows
        // before `cleared` is removing them.
        db.execute_unprepared(
            "WITH merged AS ( \
                 SELECT upstream_url, bucket_time, \
                        SUM(latency_ms_sum) AS latency_ms_sum, \
                        SUM(request_count)  AS request_count, \
                        SUM(narinfo_hits)   AS narinfo_hits, \
                        SUM(narinfo_misses) AS narinfo_misses \
                 FROM upstream_metric GROUP BY upstream_url, bucket_time \
             ), cleared AS ( DELETE FROM upstream_metric ) \
             INSERT INTO upstream_metric \
                 (id, upstream_url, bucket_time, latency_ms_sum, request_count, narinfo_hits, narinfo_misses) \
             SELECT uuidv7(), upstream_url, bucket_time, latency_ms_sum, request_count, narinfo_hits, narinfo_misses \
             FROM merged",
        )
        .await?;

        db.execute_unprepared("ALTER TABLE upstream_metric ALTER COLUMN upstream_url SET NOT NULL")
            .await?;
        db.execute_unprepared(
            "CREATE UNIQUE INDEX \"idx-upstream_metric-upstream_url-bucket_time\" \
             ON upstream_metric (upstream_url, bucket_time)",
        )
        .await?;

        db.execute_unprepared(&format!(
            "WITH merged AS ( \
                 SELECT mr.metric, mr.granularity, mr.bucket_start, cu.url AS url, \
                        SUM(mr.count) AS count, SUM(mr.sum) AS sum \
                 FROM metric_rollup mr \
                 JOIN cache_upstream cu ON cu.id::text = mr.scope->>'upstream' \
                 WHERE mr.metric IN ({UPSTREAM_METRICS}) AND cu.url IS NOT NULL \
                 GROUP BY mr.metric, mr.granularity, mr.bucket_start, cu.url \
             ), cleared AS ( \
                 DELETE FROM metric_rollup WHERE metric IN ({UPSTREAM_METRICS}) \
             ) \
             INSERT INTO metric_rollup \
                 (id, metric, granularity, bucket_start, scope, scope_hash, count, sum, min, max, sum_sq, histogram) \
             SELECT uuidv7(), metric, granularity, bucket_start, \
                    jsonb_build_object('upstream_url', url), hashtextextended(url, 0), \
                    count, sum, 0, 0, 0, NULL \
             FROM merged",
        ))
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // The URL merge is lossy and not reversible. Only the column shape is restored.
        let db = manager.get_connection();
        db.execute_unprepared(
            "DROP INDEX IF EXISTS \"idx-upstream_metric-upstream_url-bucket_time\"",
        )
        .await?;
        db.execute_unprepared("ALTER TABLE upstream_metric ADD COLUMN upstream uuid")
            .await?;
        db.execute_unprepared("ALTER TABLE upstream_metric DROP COLUMN upstream_url")
            .await?;
        Ok(())
    }
}
