/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

/// Each level is counting its dependents, locking them in id order and moving them by their edge
/// count.
pub const RIPPLE_UNWALKED_INPUTS_FN: &str = r#"
CREATE OR REPLACE FUNCTION ripple_unwalked_inputs(frontier uuid[], down boolean)
RETURNS SETOF uuid LANGUAGE plpgsql AS $$
DECLARE
    wave uuid[] := frontier;
    dependents uuid[];
    counts int[];
BEGIN
    WHILE cardinality(wave) > 0 LOOP
        SELECT coalesce(array_agg(x.derivation ORDER BY x.derivation), '{}'),
               coalesce(array_agg(x.n ORDER BY x.derivation), '{}')
          INTO dependents, counts
          FROM (SELECT e.derivation, count(*)::int AS n FROM derivation_dependency e
                 WHERE e.dependency = ANY(wave) AND e.kind IN (0, 2)
                 GROUP BY e.derivation) x;
        EXIT WHEN cardinality(dependents) = 0;
        PERFORM 1 FROM derivation WHERE id = ANY(dependents) ORDER BY id FOR NO KEY UPDATE;
        WITH moved AS (
            UPDATE derivation d
               SET unwalked_inputs = d.unwalked_inputs + CASE WHEN down THEN -c.n ELSE c.n END
              FROM unnest(dependents, counts) AS c(id, n)
             WHERE d.id = c.id
            RETURNING d.id, (d.walked AND d.unwalked_inputs = CASE WHEN down THEN 0 ELSE c.n END) AS flipped)
        SELECT coalesce(array_agg(id ORDER BY id), '{}') INTO wave FROM moved WHERE flipped;
        RETURN QUERY SELECT unnest(wave);
    END LOOP;
END $$
"#;

/// Each level is holding its dependents' advisory keys and rows in `derivation` order.
pub const RIPPLE_MISSING_RUNTIME_DEPS_FN: &str = r#"
CREATE OR REPLACE FUNCTION ripple_missing_runtime_deps(frontier uuid[], down boolean)
RETURNS SETOF uuid LANGUAGE plpgsql AS $$
DECLARE
    wave uuid[] := frontier;
    dependents uuid[];
    counts int[];
BEGIN
    WHILE cardinality(wave) > 0 LOOP
        SELECT coalesce(array_agg(x.derivation ORDER BY x.derivation), '{}'),
               coalesce(array_agg(x.n ORDER BY x.derivation), '{}')
          INTO dependents, counts
          FROM (SELECT e.derivation, count(*)::int AS n FROM derivation_dependency e
                 WHERE e.dependency = ANY(wave) AND e.kind IN (1, 2)
                 GROUP BY e.derivation) x;
        EXIT WHEN cardinality(dependents) = 0;
        PERFORM pg_advisory_xact_lock(643, k)
           FROM (SELECT DISTINCT hashtext(a::text) AS k FROM unnest(dependents) AS a ORDER BY k) keys;
        PERFORM 1 FROM derivation_build WHERE derivation = ANY(dependents) ORDER BY derivation FOR NO KEY UPDATE;
        WITH moved AS (
            UPDATE derivation_build db
               SET missing_runtime_deps = db.missing_runtime_deps + CASE WHEN down THEN -c.n ELSE c.n END
              FROM unnest(dependents, counts) AS c(derivation, n)
             WHERE db.derivation = c.derivation
            RETURNING db.derivation,
                      CASE WHEN down THEN (db.missing_runtime_deps = 0 AND (EXISTS (SELECT 1 FROM derivation_output o2 WHERE o2.derivation = db.derivation) AND NOT EXISTS (SELECT 1 FROM derivation_output o LEFT JOIN cached_path cp ON cp.hash = o.hash WHERE o.derivation = db.derivation AND cp.file_hash IS NULL)))
                           ELSE (db.missing_runtime_deps = c.n AND coalesce((SELECT bool_and(cp.file_hash IS NOT NULL) FROM derivation_output o LEFT JOIN cached_path cp ON cp.hash = o.hash WHERE o.derivation = db.derivation), false))
                      END AS flipped)
        SELECT coalesce(array_agg(derivation ORDER BY derivation), '{}') INTO wave FROM moved WHERE flipped;
        RETURN QUERY SELECT unnest(wave);
    END LOOP;
END $$
"#;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(RIPPLE_UNWALKED_INPUTS_FN).await?;
        db.execute_unprepared(RIPPLE_MISSING_RUNTIME_DEPS_FN)
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared("DROP FUNCTION IF EXISTS ripple_unwalked_inputs(uuid[], boolean)")
            .await?;
        db.execute_unprepared(
            "DROP FUNCTION IF EXISTS ripple_missing_runtime_deps(uuid[], boolean)",
        )
        .await?;

        Ok(())
    }
}
