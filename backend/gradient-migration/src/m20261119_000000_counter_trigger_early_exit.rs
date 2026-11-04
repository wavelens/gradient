/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &str = r#"
CREATE OR REPLACE FUNCTION public.evaluation_shared_build_moved() RETURNS trigger
    LANGUAGE plpgsql
    AS $$ BEGIN IF NOT EXISTS (SELECT id, status, wanted FROM new_rows EXCEPT SELECT id, status, wanted FROM old_rows) THEN RETURN NULL; END IF; INSERT INTO evaluation_shared_build_delta (evaluation, named, active, failed, queued, building) SELECT m.evaluation, 0, m.active, m.failed, m.queued, m.building FROM (SELECT d.evaluation, sum(d.active)::int AS active, sum(d.failed)::int AS failed, sum(d.queued)::int AS queued, sum(d.building)::int AS building FROM (SELECT bj.evaluation, c.active, c.failed, c.queued, c.building FROM new_rows n CROSS JOIN LATERAL evaluation_shared_build_counts(n.status, n.wanted) c JOIN build_job bj ON bj.derivation_build = n.id UNION ALL SELECT bj.evaluation, -c.active, -c.failed, -c.queued, -c.building FROM old_rows o CROSS JOIN LATERAL evaluation_shared_build_counts(o.status, o.wanted) c JOIN build_job bj ON bj.derivation_build = o.id) d GROUP BY d.evaluation) m JOIN evaluation e ON e.id = m.evaluation WHERE e.status IN (0, 8, 1, 2, 3, 4) AND (m.active, m.failed, m.queued, m.building) <> (0, 0, 0, 0); RETURN NULL; END $$;
"#;

// The old and new transition tables are summed separately: a join between the two has no row
// estimate and the plan PL/pgSQL caches for it after a one-row update loops over every pair.

const DOWN: &str = r#"
CREATE OR REPLACE FUNCTION public.evaluation_shared_build_moved() RETURNS trigger
    LANGUAGE plpgsql
    AS $$ BEGIN INSERT INTO evaluation_shared_build_delta (evaluation, named, active, failed, queued, building) SELECT m.evaluation, 0, m.active, m.failed, m.queued, m.building FROM (SELECT d.evaluation, sum(d.active)::int AS active, sum(d.failed)::int AS failed, sum(d.queued)::int AS queued, sum(d.building)::int AS building FROM (SELECT bj.evaluation, c.active, c.failed, c.queued, c.building FROM new_rows n CROSS JOIN LATERAL evaluation_shared_build_counts(n.status, n.wanted) c JOIN build_job bj ON bj.derivation_build = n.id UNION ALL SELECT bj.evaluation, -c.active, -c.failed, -c.queued, -c.building FROM old_rows o CROSS JOIN LATERAL evaluation_shared_build_counts(o.status, o.wanted) c JOIN build_job bj ON bj.derivation_build = o.id) d GROUP BY d.evaluation) m JOIN evaluation e ON e.id = m.evaluation WHERE e.status IN (0, 8, 1, 2, 3, 4) AND (m.active, m.failed, m.queued, m.building) <> (0, 0, 0, 0); RETURN NULL; END $$;
"#;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(DOWN).await?;
        Ok(())
    }
}
