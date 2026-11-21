/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;

use crate::sql_on_one_connection::run_sql_statements_on_one_connection;

const LOCK_TIMEOUT_SQL: &str = "SET lock_timeout = '5s';";

// The statements run outside a transaction. The NOT VALID check is before the DELETE, so no new row
// without these values can enter in between. Writes continue during the VALIDATE scan. A later
// release can set NOT NULL from the validated check without another scan.
const ADD_CACHED_PATH_FILE_HASH_NAR_HASH_NAR_SIZE_NOT_NULL_CHECK_NOT_VALID_SQL: &str = "ALTER TABLE public.cached_path \
    DROP CONSTRAINT IF EXISTS cached_path_file_hash_nar_hash_nar_size_not_null, \
    ADD CONSTRAINT cached_path_file_hash_nar_hash_nar_size_not_null \
    CHECK (file_hash IS NOT NULL AND nar_hash IS NOT NULL AND nar_size IS NOT NULL) NOT VALID;";

const DELETE_CACHED_PATHS_WITHOUT_FILE_HASH_NAR_HASH_OR_NAR_SIZE_AND_MARK_AFFECTED_DERIVATIONS_NOT_WALKED_SQL: &str = r#"
DO $$
DECLARE
    deleted_hashes text[];
    owners uuid[];
    complete uuid[];
BEGIN
    WITH deleted AS (
        DELETE FROM public.cached_path
         WHERE file_hash IS NULL OR nar_hash IS NULL OR nar_size IS NULL
        RETURNING hash)
    SELECT coalesce(array_agg(DISTINCT hash), '{}') INTO deleted_hashes FROM deleted;
    SELECT coalesce(array_agg(id ORDER BY id), '{}') INTO owners FROM (
        SELECT id FROM public.derivation WHERE hash = ANY(deleted_hashes)
        UNION SELECT derivation FROM public.derivation_input_source WHERE hash = ANY(deleted_hashes)) o;
    PERFORM 1 FROM public.derivation WHERE id = ANY(owners) ORDER BY id FOR NO KEY UPDATE;
    SELECT coalesce(array_agg(id ORDER BY id), '{}') INTO complete
      FROM public.derivation WHERE id = ANY(owners) AND walked AND unwalked_inputs = 0;
    UPDATE public.derivation SET walked = false WHERE id = ANY(owners) AND walked;
    PERFORM public.ripple_unwalked_inputs(complete, false);
END $$;
"#;

const SQL_VALIDATE_CACHED_PATH_FILE_HASH_NAR_HASH_AND_NAR_SIZE_NOT_NULL_CHECK: &str = "ALTER TABLE public.cached_path \
    VALIDATE CONSTRAINT cached_path_file_hash_nar_hash_nar_size_not_null;";

const DROP_CACHED_PATH_FILE_HASH_NAR_HASH_NAR_SIZE_NOT_NULL_CHECK_CONSTRAINT_SQL: &str = "ALTER TABLE public.cached_path \
    DROP CONSTRAINT IF EXISTS cached_path_file_hash_nar_hash_nar_size_not_null;";

const RESET_LOCK_TIMEOUT_SQL: &str = "RESET lock_timeout;";

pub(crate) const UP: &str = r#"
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
                      CASE WHEN down THEN (db.missing_runtime_deps = 0 AND (EXISTS (SELECT 1 FROM derivation_output o2 WHERE o2.derivation = db.derivation) AND NOT EXISTS (SELECT 1 FROM derivation_output o WHERE o.derivation = db.derivation AND o.name <> ALL(db.unneeded_outputs) AND NOT EXISTS (SELECT 1 FROM cached_path cp WHERE cp.hash = o.hash))))
                           ELSE (db.missing_runtime_deps = c.n AND coalesce((SELECT bool_and(cp.id IS NOT NULL OR o.name = ANY(db.unneeded_outputs)) FROM derivation_output o LEFT JOIN cached_path cp ON cp.hash = o.hash WHERE o.derivation = db.derivation), false))
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
                      CASE WHEN down THEN (db.missing_runtime_deps = 0 AND (EXISTS (SELECT 1 FROM derivation_output o2 WHERE o2.derivation = db.derivation) AND NOT EXISTS (SELECT 1 FROM derivation_output o LEFT JOIN cached_path cp ON cp.hash = o.hash WHERE o.derivation = db.derivation AND cp.file_hash IS NULL AND o.name <> ALL(db.unneeded_outputs))))
                           ELSE (db.missing_runtime_deps = c.n AND coalesce((SELECT bool_and(cp.file_hash IS NOT NULL OR o.name = ANY(db.unneeded_outputs)) FROM derivation_output o LEFT JOIN cached_path cp ON cp.hash = o.hash WHERE o.derivation = db.derivation), false))
                      END AS flipped)
        SELECT coalesce(array_agg(derivation ORDER BY derivation), '{}') INTO wave FROM moved WHERE flipped;
        RETURN QUERY SELECT unnest(wave);
    END LOOP;
END $$;
"#;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    fn use_transaction(&self) -> Option<bool> {
        Some(false)
    }

    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        run_sql_statements_on_one_connection(
            manager,
            &[
                LOCK_TIMEOUT_SQL,
                ADD_CACHED_PATH_FILE_HASH_NAR_HASH_NAR_SIZE_NOT_NULL_CHECK_NOT_VALID_SQL,
                DELETE_CACHED_PATHS_WITHOUT_FILE_HASH_NAR_HASH_OR_NAR_SIZE_AND_MARK_AFFECTED_DERIVATIONS_NOT_WALKED_SQL,
                SQL_VALIDATE_CACHED_PATH_FILE_HASH_NAR_HASH_AND_NAR_SIZE_NOT_NULL_CHECK,
                UP,
                RESET_LOCK_TIMEOUT_SQL,
            ],
        )
        .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        run_sql_statements_on_one_connection(
            manager,
            &[
                LOCK_TIMEOUT_SQL,
                DROP_CACHED_PATH_FILE_HASH_NAR_HASH_NAR_SIZE_NOT_NULL_CHECK_CONSTRAINT_SQL,
                DOWN,
                RESET_LOCK_TIMEOUT_SQL,
            ],
        )
        .await
    }
}
