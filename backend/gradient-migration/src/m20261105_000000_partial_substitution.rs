/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

pub(crate) const UP: &str = r#"
ALTER TABLE public.derivation_dependency ADD COLUMN dependency_outputs text[];
ALTER TABLE public.derivation_build ADD COLUMN unneeded_outputs text[] DEFAULT '{}'::text[] NOT NULL;
CREATE INDEX "idx-derivation_build-unneeded-outputs" ON public.derivation_build USING btree (derivation) WHERE (unneeded_outputs <> '{}'::text[]);

CREATE OR REPLACE FUNCTION public.ripple_missing_runtime_deps(frontier uuid[], down boolean) RETURNS SETOF uuid
    LANGUAGE plpgsql
    AS $$
DECLARE
    wave uuid[] := frontier;
    parents uuid[];
    counts int[];
BEGIN
    WHILE cardinality(wave) > 0 LOOP
        SELECT coalesce(array_agg(x.derivation ORDER BY x.derivation), '{}'),
               coalesce(array_agg(x.n ORDER BY x.derivation), '{}')
          INTO parents, counts
          FROM (SELECT e.derivation, count(*)::int AS n FROM derivation_dependency e
                 WHERE e.dependency = ANY(wave) AND e.kind IN (1, 2)
                 GROUP BY e.derivation) x;
        EXIT WHEN cardinality(parents) = 0;
        PERFORM pg_advisory_xact_lock(643, k)
           FROM (SELECT DISTINCT hashtext(a::text) AS k FROM unnest(parents) AS a ORDER BY k) keys;
        PERFORM 1 FROM derivation_build WHERE derivation = ANY(parents) ORDER BY derivation FOR NO KEY UPDATE;
        WITH moved AS (
            UPDATE derivation_build db
               SET missing_runtime_deps = db.missing_runtime_deps + CASE WHEN down THEN -c.n ELSE c.n END
              FROM unnest(parents, counts) AS c(derivation, n)
             WHERE db.derivation = c.derivation
            RETURNING db.derivation,
                      CASE WHEN down THEN (db.missing_runtime_deps = 0 AND (EXISTS (SELECT 1 FROM derivation_output o2 WHERE o2.derivation = db.derivation) AND NOT EXISTS (SELECT 1 FROM derivation_output o LEFT JOIN cached_path cp ON cp.hash = o.hash WHERE o.derivation = db.derivation AND cp.file_hash IS NULL AND o.name <> ALL(db.unneeded_outputs))))
                           ELSE (db.missing_runtime_deps = c.n AND coalesce((SELECT bool_and(cp.file_hash IS NOT NULL OR o.name = ANY(db.unneeded_outputs)) FROM derivation_output o LEFT JOIN cached_path cp ON cp.hash = o.hash WHERE o.derivation = db.derivation), false))
                      END AS flipped)
        SELECT coalesce(array_agg(derivation ORDER BY derivation), '{}') INTO wave FROM moved WHERE flipped;
        RETURN QUERY SELECT unnest(wave);
    END LOOP;
END $$;
"#;

const DOWN: &str = r#"
CREATE OR REPLACE FUNCTION public.ripple_missing_runtime_deps(frontier uuid[], down boolean) RETURNS SETOF uuid
    LANGUAGE plpgsql
    AS $$
DECLARE
    wave uuid[] := frontier;
    parents uuid[];
    counts int[];
BEGIN
    WHILE cardinality(wave) > 0 LOOP
        SELECT coalesce(array_agg(x.derivation ORDER BY x.derivation), '{}'),
               coalesce(array_agg(x.n ORDER BY x.derivation), '{}')
          INTO parents, counts
          FROM (SELECT e.derivation, count(*)::int AS n FROM derivation_dependency e
                 WHERE e.dependency = ANY(wave) AND e.kind IN (1, 2)
                 GROUP BY e.derivation) x;
        EXIT WHEN cardinality(parents) = 0;
        PERFORM pg_advisory_xact_lock(643, k)
           FROM (SELECT DISTINCT hashtext(a::text) AS k FROM unnest(parents) AS a ORDER BY k) keys;
        PERFORM 1 FROM derivation_build WHERE derivation = ANY(parents) ORDER BY derivation FOR NO KEY UPDATE;
        WITH moved AS (
            UPDATE derivation_build db
               SET missing_runtime_deps = db.missing_runtime_deps + CASE WHEN down THEN -c.n ELSE c.n END
              FROM unnest(parents, counts) AS c(derivation, n)
             WHERE db.derivation = c.derivation
            RETURNING db.derivation,
                      CASE WHEN down THEN (db.missing_runtime_deps = 0 AND (EXISTS (SELECT 1 FROM derivation_output o2 WHERE o2.derivation = db.derivation) AND NOT EXISTS (SELECT 1 FROM derivation_output o LEFT JOIN cached_path cp ON cp.hash = o.hash WHERE o.derivation = db.derivation AND cp.file_hash IS NULL)))
                           ELSE (db.missing_runtime_deps = c.n AND coalesce((SELECT bool_and(cp.file_hash IS NOT NULL) FROM derivation_output o LEFT JOIN cached_path cp ON cp.hash = o.hash WHERE o.derivation = db.derivation), false))
                      END AS flipped)
        SELECT coalesce(array_agg(derivation ORDER BY derivation), '{}') INTO wave FROM moved WHERE flipped;
        RETURN QUERY SELECT unnest(wave);
    END LOOP;
END $$;
DROP INDEX public."idx-derivation_build-unneeded-outputs";
ALTER TABLE public.derivation_build DROP COLUMN unneeded_outputs;
ALTER TABLE public.derivation_dependency DROP COLUMN dependency_outputs;
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
