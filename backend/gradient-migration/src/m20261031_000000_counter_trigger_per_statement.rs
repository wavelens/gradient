/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &str = r#"
DROP TRIGGER evaluation_shared_build_moved ON public.derivation_build;
CREATE OR REPLACE FUNCTION public.evaluation_shared_build_moved() RETURNS trigger
    LANGUAGE plpgsql
    AS $$ BEGIN INSERT INTO evaluation_shared_build_delta (evaluation, named, active, failed, queued, building) SELECT m.evaluation, 0, m.active, m.failed, m.queued, m.building FROM (SELECT d.evaluation, sum(d.active)::int AS active, sum(d.failed)::int AS failed, sum(d.queued)::int AS queued, sum(d.building)::int AS building FROM (SELECT bj.evaluation, c.active, c.failed, c.queued, c.building FROM new_rows n CROSS JOIN LATERAL evaluation_shared_build_counts(n.status, n.wanted) c JOIN build_job bj ON bj.derivation_build = n.id UNION ALL SELECT bj.evaluation, -c.active, -c.failed, -c.queued, -c.building FROM old_rows o CROSS JOIN LATERAL evaluation_shared_build_counts(o.status, o.wanted) c JOIN build_job bj ON bj.derivation_build = o.id) d GROUP BY d.evaluation) m JOIN evaluation e ON e.id = m.evaluation WHERE e.status IN (0, 8, 1, 2, 3, 4) AND (m.active, m.failed, m.queued, m.building) <> (0, 0, 0, 0); RETURN NULL; END $$;
CREATE TRIGGER evaluation_shared_build_moved AFTER UPDATE ON public.derivation_build REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION public.evaluation_shared_build_moved();
"#;

// The old and new transition tables are summed separately: a join between the two has no row
// estimate and the plan PL/pgSQL caches for it after a one-row update loops over every pair.

const DOWN: &str = r#"
DROP TRIGGER evaluation_shared_build_moved ON public.derivation_build;
CREATE OR REPLACE FUNCTION public.evaluation_shared_build_moved() RETURNS trigger
    LANGUAGE plpgsql
    AS $$ BEGIN INSERT INTO evaluation_shared_build_delta (evaluation, named, active, failed, queued, building) SELECT bj.evaluation, 0, nc.active - oc.active, nc.failed - oc.failed, nc.queued - oc.queued, nc.building - oc.building FROM evaluation_shared_build_counts(NEW.status, NEW.wanted) nc, evaluation_shared_build_counts(OLD.status, OLD.wanted) oc, build_job bj JOIN evaluation e ON e.id = bj.evaluation WHERE bj.derivation_build = NEW.id AND e.status IN (0, 8, 1, 2, 3, 4) AND (nc.active, nc.failed, nc.queued, nc.building) IS DISTINCT FROM (oc.active, oc.failed, oc.queued, oc.building); RETURN NULL; END $$;
CREATE TRIGGER evaluation_shared_build_moved AFTER UPDATE OF status, wanted ON public.derivation_build FOR EACH ROW WHEN (((old.status IS DISTINCT FROM new.status) OR (old.wanted IS DISTINCT FROM new.wanted))) EXECUTE FUNCTION public.evaluation_shared_build_moved();
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
