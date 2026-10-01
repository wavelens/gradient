/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Renames every table, column, index, function, trigger and stored value that
//! carried an abstract concept name (anchor, demanded, substitutable, readiness,
//! forge, outbox) to its plain name. Function bodies do not follow a rename, so
//! the counter and ripple functions are replaced; Postgres 17+ NOT NULL
//! constraints are renamed by a final sweep over the affected tables. Stored
//! values are rewritten first, touching only the rows that carry an old value,
//! so the renames' ACCESS EXCLUSIVE locks are held only for the catalog work.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

pub const SHARED_BUILD_COUNTS_FN: &str = "CREATE OR REPLACE FUNCTION evaluation_shared_build_counts(\
     status integer, wanted boolean, \
     OUT active integer, OUT failed integer, OUT queued integer, OUT building integer) \
     LANGUAGE sql IMMUTABLE AS $$ SELECT \
     ((evaluation_shared_build_counts.status IN (0, 1, 2, 8, 10, 5) AND (evaluation_shared_build_counts.wanted OR evaluation_shared_build_counts.status IN (1, 2))))::int, \
     (evaluation_shared_build_counts.status IN (4, 5, 6, 9))::int, \
     (evaluation_shared_build_counts.status = 1)::int, \
     (evaluation_shared_build_counts.status = 2)::int $$";

macro_rules! live {
    () => {
        "e.status IN (0, 8, 1, 2, 3, 4)"
    };
}

const MOVED_FN: &str = concat!(
    "CREATE OR REPLACE FUNCTION evaluation_shared_build_moved() RETURNS trigger \
     LANGUAGE plpgsql AS $$ BEGIN \
     INSERT INTO evaluation_shared_build_delta (evaluation, named, active, failed, queued, building) \
     SELECT bj.evaluation, 0, nc.active - oc.active, nc.failed - oc.failed, \
            nc.queued - oc.queued, nc.building - oc.building \
     FROM evaluation_shared_build_counts(NEW.status, NEW.wanted) nc, \
          evaluation_shared_build_counts(OLD.status, OLD.wanted) oc, \
          build_job bj JOIN evaluation e ON e.id = bj.evaluation \
     WHERE bj.derivation_build = NEW.id AND ",
    live!(),
    " AND (nc.active, nc.failed, nc.queued, nc.building) \
           IS DISTINCT FROM (oc.active, oc.failed, oc.queued, oc.building); \
     RETURN NULL; END $$"
);

const NAMED_FN: &str = concat!(
    "CREATE OR REPLACE FUNCTION evaluation_shared_build_named() RETURNS trigger \
     LANGUAGE plpgsql AS $$ BEGIN \
     INSERT INTO evaluation_shared_build_delta (evaluation, named, active, failed, queued, building) \
     SELECT n.evaluation, count(*)::int, sum(c.active)::int, sum(c.failed)::int, \
            sum(c.queued)::int, sum(c.building)::int \
     FROM new_rows n JOIN evaluation e ON e.id = n.evaluation \
     JOIN derivation_build db ON db.id = n.derivation_build \
     CROSS JOIN LATERAL evaluation_shared_build_counts(db.status, db.wanted) c \
     WHERE ",
    live!(),
    " GROUP BY n.evaluation; \
     RETURN NULL; END $$"
);

const UNNAMED_FN: &str = concat!(
    "CREATE OR REPLACE FUNCTION evaluation_shared_build_unnamed() RETURNS trigger \
     LANGUAGE plpgsql AS $$ BEGIN \
     INSERT INTO evaluation_shared_build_delta (evaluation, named, active, failed, queued, building) \
     SELECT o.evaluation, -count(*)::int, -coalesce(sum(c.active), 0)::int, \
            -coalesce(sum(c.failed), 0)::int, -coalesce(sum(c.queued), 0)::int, \
            -coalesce(sum(c.building), 0)::int \
     FROM old_rows o JOIN evaluation e ON e.id = o.evaluation \
     LEFT JOIN derivation_build db ON db.id = o.derivation_build \
     LEFT JOIN LATERAL evaluation_shared_build_counts(db.status, db.wanted) c ON db.id IS NOT NULL \
     WHERE ",
    live!(),
    " GROUP BY o.evaluation; \
     RETURN NULL; END $$"
);

/// `derivation.unwalked_inputs` over build edges, unchanged but for the name of
/// the level's parents.
pub const RIPPLE_UNWALKED_INPUTS_FN: &str = r#"
CREATE OR REPLACE FUNCTION ripple_unwalked_inputs(frontier uuid[], down boolean)
RETURNS SETOF uuid LANGUAGE plpgsql AS $$
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
                 WHERE e.dependency = ANY(wave) AND e.kind IN (0, 2)
                 GROUP BY e.derivation) x;
        EXIT WHEN cardinality(parents) = 0;
        PERFORM 1 FROM derivation WHERE id = ANY(parents) ORDER BY id FOR NO KEY UPDATE;
        WITH moved AS (
            UPDATE derivation d
               SET unwalked_inputs = d.unwalked_inputs + CASE WHEN down THEN -c.n ELSE c.n END
              FROM unnest(parents, counts) AS c(id, n)
             WHERE d.id = c.id
            RETURNING d.id, (d.walked AND d.unwalked_inputs = CASE WHEN down THEN 0 ELSE c.n END) AS flipped)
        SELECT coalesce(array_agg(id ORDER BY id), '{}') INTO wave FROM moved WHERE flipped;
        RETURN QUERY SELECT unnest(wave);
    END LOOP;
END $$;
"#;

/// `derivation_build.missing_runtime_deps` over runtime edges, unchanged but for
/// the name of the level's parents.
pub const RIPPLE_MISSING_RUNTIME_DEPS_FN: &str = r#"
CREATE OR REPLACE FUNCTION ripple_missing_runtime_deps(frontier uuid[], down boolean)
RETURNS SETOF uuid LANGUAGE plpgsql AS $$
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
"#;

const RENAMES_UP: &str = r#"
ALTER TABLE outbox RENAME TO pending_delivery;
ALTER TABLE pending_delivery RENAME CONSTRAINT outbox_pkey TO pending_delivery_pkey;
ALTER INDEX "idx-outbox-due" RENAME TO "idx-pending_delivery-due";
ALTER INDEX "idx-outbox-key" RENAME TO "idx-pending_delivery-key";
ALTER INDEX "idx-outbox-settled" RENAME TO "idx-pending_delivery-settled";

ALTER TABLE evaluation_anchor_delta RENAME TO evaluation_shared_build_delta;
ALTER TABLE evaluation_shared_build_delta RENAME CONSTRAINT evaluation_anchor_delta_pkey TO evaluation_shared_build_delta_pkey;
ALTER SEQUENCE evaluation_anchor_delta_id_seq RENAME TO evaluation_shared_build_delta_id_seq;
ALTER INDEX "idx-evaluation_anchor_delta-evaluation" RENAME TO "idx-evaluation_shared_build_delta-evaluation";

ALTER TABLE evaluation RENAME COLUMN named_anchors TO named_shared_builds;
ALTER TABLE evaluation RENAME COLUMN active_anchors TO active_shared_builds;
ALTER TABLE evaluation RENAME COLUMN failed_anchors TO failed_shared_builds;
ALTER TABLE evaluation RENAME COLUMN queued_anchors TO queued_shared_builds;
ALTER TABLE evaluation RENAME COLUMN building_anchors TO building_shared_builds;

ALTER TABLE derivation_build RENAME COLUMN demanded TO wanted;
ALTER TABLE derivation_build RENAME COLUMN substitutable TO cache_available;
ALTER TABLE derivation_build RENAME COLUMN unready_deps TO blocking_deps;
ALTER TABLE integration RENAME COLUMN forge_type TO git_host_type;
ALTER TABLE open_pr_state RENAME COLUMN forge_pr_number TO git_host_pr_number;

ALTER INDEX "idx-build_job-evaluation-anchor" RENAME TO "idx-build_job-evaluation-shared_build";
ALTER INDEX "idx-derivation_build-dispatch-ready" RENAME TO "idx-derivation_build-dispatch-startable";
ALTER INDEX "idx-derivation_build-fetchable-unwhole" RENAME TO "idx-derivation_build-fetchable-incomplete";
ALTER STATISTICS "stat-derivation_build-readiness" RENAME TO "stat-derivation_build-can_start";

ALTER FUNCTION evaluation_anchor_moved() RENAME TO evaluation_shared_build_moved;
ALTER FUNCTION evaluation_anchor_named() RENAME TO evaluation_shared_build_named;
ALTER FUNCTION evaluation_anchor_unnamed() RENAME TO evaluation_shared_build_unnamed;
ALTER TRIGGER evaluation_anchor_moved ON derivation_build RENAME TO evaluation_shared_build_moved;
ALTER TRIGGER evaluation_anchor_named ON build_job RENAME TO evaluation_shared_build_named;
ALTER TRIGGER evaluation_anchor_unnamed ON build_job RENAME TO evaluation_shared_build_unnamed;
"#;

const DATA_UP: &str = r#"
UPDATE task_action SET config = jsonb_set(config, '{type}', '"git_host_status_report"')
 WHERE config->>'type' = 'forge_status_report';
UPDATE evaluation
   SET waiting_reason = (waiting_reason - 'pending_anchors')
       || jsonb_build_object('pending_shared_builds', waiting_reason->'pending_anchors')
 WHERE waiting_reason ? 'pending_anchors';
UPDATE webhook SET events = (
         SELECT jsonb_agg(CASE WHEN e = '"graph.ingested"'::jsonb THEN '"graph.recorded"'::jsonb ELSE e END)
           FROM jsonb_array_elements(events) e)
 WHERE events @> '["graph.ingested"]';
UPDATE task_action SET events = (
         SELECT jsonb_agg(CASE WHEN e = '"graph.ingested"'::jsonb THEN '"graph.recorded"'::jsonb ELSE e END)
           FROM jsonb_array_elements(events) e)
 WHERE jsonb_typeof(events) = 'array' AND events @> '["graph.ingested"]';
UPDATE webhook_delivery SET event = 'graph.recorded' WHERE event = 'graph.ingested';
UPDATE metric_rollup SET metric = replace(metric, '.substitute_relay.', '.substitute_passthrough.')
 WHERE metric LIKE 'phase.%.substitute\_relay.ms';
"#;

const DATA_DOWN: &str = r#"
UPDATE metric_rollup SET metric = replace(metric, '.substitute_passthrough.', '.substitute_relay.')
 WHERE metric LIKE 'phase.%.substitute\_passthrough.ms';
UPDATE webhook_delivery SET event = 'graph.ingested' WHERE event = 'graph.recorded';
UPDATE task_action SET events = (
         SELECT jsonb_agg(CASE WHEN e = '"graph.recorded"'::jsonb THEN '"graph.ingested"'::jsonb ELSE e END)
           FROM jsonb_array_elements(events) e)
 WHERE jsonb_typeof(events) = 'array' AND events @> '["graph.recorded"]';
UPDATE webhook SET events = (
         SELECT jsonb_agg(CASE WHEN e = '"graph.recorded"'::jsonb THEN '"graph.ingested"'::jsonb ELSE e END)
           FROM jsonb_array_elements(events) e)
 WHERE events @> '["graph.recorded"]';
UPDATE evaluation
   SET waiting_reason = (waiting_reason - 'pending_shared_builds')
       || jsonb_build_object('pending_anchors', waiting_reason->'pending_shared_builds')
 WHERE waiting_reason ? 'pending_shared_builds';
UPDATE task_action SET config = jsonb_set(config, '{type}', '"forge_status_report"')
 WHERE config->>'type' = 'git_host_status_report';
"#;

const RENAMES_DOWN: &str = r#"
ALTER TRIGGER evaluation_shared_build_unnamed ON build_job RENAME TO evaluation_anchor_unnamed;
ALTER TRIGGER evaluation_shared_build_named ON build_job RENAME TO evaluation_anchor_named;
ALTER TRIGGER evaluation_shared_build_moved ON derivation_build RENAME TO evaluation_anchor_moved;
ALTER FUNCTION evaluation_shared_build_unnamed() RENAME TO evaluation_anchor_unnamed;
ALTER FUNCTION evaluation_shared_build_named() RENAME TO evaluation_anchor_named;
ALTER FUNCTION evaluation_shared_build_moved() RENAME TO evaluation_anchor_moved;

ALTER STATISTICS "stat-derivation_build-can_start" RENAME TO "stat-derivation_build-readiness";
ALTER INDEX "idx-derivation_build-fetchable-incomplete" RENAME TO "idx-derivation_build-fetchable-unwhole";
ALTER INDEX "idx-derivation_build-dispatch-startable" RENAME TO "idx-derivation_build-dispatch-ready";
ALTER INDEX "idx-build_job-evaluation-shared_build" RENAME TO "idx-build_job-evaluation-anchor";

ALTER TABLE open_pr_state RENAME COLUMN git_host_pr_number TO forge_pr_number;
ALTER TABLE integration RENAME COLUMN git_host_type TO forge_type;
ALTER TABLE derivation_build RENAME COLUMN blocking_deps TO unready_deps;
ALTER TABLE derivation_build RENAME COLUMN cache_available TO substitutable;
ALTER TABLE derivation_build RENAME COLUMN wanted TO demanded;

ALTER TABLE evaluation RENAME COLUMN building_shared_builds TO building_anchors;
ALTER TABLE evaluation RENAME COLUMN queued_shared_builds TO queued_anchors;
ALTER TABLE evaluation RENAME COLUMN failed_shared_builds TO failed_anchors;
ALTER TABLE evaluation RENAME COLUMN active_shared_builds TO active_anchors;
ALTER TABLE evaluation RENAME COLUMN named_shared_builds TO named_anchors;

ALTER INDEX "idx-evaluation_shared_build_delta-evaluation" RENAME TO "idx-evaluation_anchor_delta-evaluation";
ALTER SEQUENCE evaluation_shared_build_delta_id_seq RENAME TO evaluation_anchor_delta_id_seq;
ALTER TABLE evaluation_shared_build_delta RENAME CONSTRAINT evaluation_shared_build_delta_pkey TO evaluation_anchor_delta_pkey;
ALTER TABLE evaluation_shared_build_delta RENAME TO evaluation_anchor_delta;

ALTER INDEX "idx-pending_delivery-settled" RENAME TO "idx-outbox-settled";
ALTER INDEX "idx-pending_delivery-key" RENAME TO "idx-outbox-key";
ALTER INDEX "idx-pending_delivery-due" RENAME TO "idx-outbox-due";
ALTER TABLE pending_delivery RENAME CONSTRAINT pending_delivery_pkey TO outbox_pkey;
ALTER TABLE pending_delivery RENAME TO outbox;
"#;

/// Renames every NOT NULL constraint on the affected tables to the name Postgres
/// would give it today, `<table>_<column>_not_null`.
const NOT_NULL_SWEEP: &str = r#"
DO $$
DECLARE r record;
BEGIN
    FOR r IN
        SELECT t.relname AS tbl, c.conname AS name,
               t.relname || '_' || a.attname || '_not_null' AS wanted_name
        FROM pg_constraint c
        JOIN pg_class t ON t.oid = c.conrelid
        JOIN pg_namespace n ON n.oid = t.relnamespace
        JOIN pg_attribute a ON a.attrelid = t.oid AND a.attnum = c.conkey[1]
        WHERE n.nspname = 'public'
          AND c.contype = 'n'
          AND c.conname ~ '(anchor|outbox|demanded|substitutable|unready_deps|wanted|cache_available|blocking_deps|shared_build|pending_delivery|forge_type|git_host_type)'
          AND t.relname IN ('pending_delivery', 'outbox', 'evaluation_shared_build_delta',
                            'evaluation_anchor_delta', 'evaluation', 'derivation_build',
                            'integration')
          AND c.conname <> t.relname || '_' || a.attname || '_not_null'
    LOOP
        EXECUTE format('ALTER TABLE public.%I RENAME CONSTRAINT %I TO %I',
                       r.tbl, r.name, r.wanted_name);
    END LOOP;
END $$;
"#;

fn old_name(sql: &str) -> String {
    sql.replace("evaluation_shared_build_", "evaluation_anchor_")
        .replace("wanted", "demanded")
        .replace("parents", "dependents")
}

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(DATA_UP).await?;
        db.execute_unprepared(RENAMES_UP).await?;
        db.execute_unprepared(SHARED_BUILD_COUNTS_FN).await?;
        for f in [MOVED_FN, NAMED_FN, UNNAMED_FN] {
            db.execute_unprepared(f).await?;
        }
        db.execute_unprepared("DROP FUNCTION evaluation_anchor_counts(integer, boolean)")
            .await?;
        db.execute_unprepared(RIPPLE_UNWALKED_INPUTS_FN).await?;
        db.execute_unprepared(RIPPLE_MISSING_RUNTIME_DEPS_FN)
            .await?;
        db.execute_unprepared(NOT_NULL_SWEEP).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(DATA_DOWN).await?;
        db.execute_unprepared(RENAMES_DOWN).await?;
        db.execute_unprepared(&old_name(SHARED_BUILD_COUNTS_FN))
            .await?;
        for f in [MOVED_FN, NAMED_FN, UNNAMED_FN] {
            db.execute_unprepared(&old_name(f)).await?;
        }
        db.execute_unprepared("DROP FUNCTION evaluation_shared_build_counts(integer, boolean)")
            .await?;
        db.execute_unprepared(&old_name(RIPPLE_UNWALKED_INPUTS_FN))
            .await?;
        db.execute_unprepared(&old_name(RIPPLE_MISSING_RUNTIME_DEPS_FN))
            .await?;
        db.execute_unprepared(NOT_NULL_SWEEP).await?;
        Ok(())
    }
}
