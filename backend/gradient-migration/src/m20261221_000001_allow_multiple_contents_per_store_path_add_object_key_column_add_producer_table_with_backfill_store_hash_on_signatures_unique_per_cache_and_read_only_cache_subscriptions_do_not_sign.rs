/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

// Servers built from main before this change insert `cached_path` rows without
// `nar_object_storage_key` during a rolling upgrade. A trigger can fill the missing value from
// `hash`. The trigger is absent from the next release.
const UP: &str = r#"
SET LOCAL lock_timeout = '5s';

ALTER TABLE public.cached_path DROP CONSTRAINT cached_path_hash_key;
ALTER TABLE public.cached_path
    ADD COLUMN nar_object_storage_key text,
    ADD COLUMN server_computed_hashes_and_sizes boolean NOT NULL DEFAULT false;
UPDATE public.cached_path SET nar_object_storage_key = hash;
ALTER TABLE public.cached_path ALTER COLUMN nar_object_storage_key SET NOT NULL;
CREATE FUNCTION public.set_nar_object_storage_key_to_row_hash_when_null()
    RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    NEW.nar_object_storage_key := COALESCE(NEW.nar_object_storage_key, NEW.hash);
    RETURN NEW;
END
$$;
CREATE TRIGGER "trg-cached_path-nar_object_storage_key"
    BEFORE INSERT ON public.cached_path
    FOR EACH ROW EXECUTE FUNCTION public.set_nar_object_storage_key_to_row_hash_when_null();

CREATE TABLE public.store_path_content_producer (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    cached_path uuid NOT NULL REFERENCES public.cached_path(id) ON DELETE CASCADE,
    kind integer NOT NULL,
    worker_registration uuid REFERENCES public.worker_registration(id) ON DELETE SET NULL,
    team_worker uuid REFERENCES public.team_worker(id) ON DELETE SET NULL,
    dispatched_job uuid,
    cache uuid REFERENCES public.cache(id) ON DELETE CASCADE,
    uploaded_by_user uuid REFERENCES public."user"(id) ON DELETE SET NULL,
    uploaded_by_api_key uuid REFERENCES public.api(id) ON DELETE SET NULL,
    upload_channel integer,
    uploading_member_project_id uuid REFERENCES public.project(id) ON DELETE CASCADE,
    created_at timestamp without time zone NOT NULL DEFAULT (now() AT TIME ZONE 'utc'),
    CONSTRAINT "chk-store_path_content_producer-producer_names_one_source" CHECK (
        kind IN (0, 1, 2, 3) AND (kind = 2) = (uploading_member_project_id IS NOT NULL) AND CASE kind
            WHEN 0 THEN num_nonnulls(worker_registration, team_worker) <= 1 AND cache IS NULL
            WHEN 1 THEN num_nonnulls(worker_registration, team_worker) = 0 AND cache IS NOT NULL
            WHEN 2 THEN num_nonnulls(worker_registration, team_worker, cache) = 0
            WHEN 3 THEN num_nonnulls(worker_registration, team_worker, cache) = 0
        END
    )
);
CREATE UNIQUE INDEX "uq-store_path_content_producer-source"
    ON public.store_path_content_producer
    (cached_path, kind, worker_registration, team_worker, cache, uploading_member_project_id)
    NULLS NOT DISTINCT
    WHERE kind <> 0 OR worker_registration IS NOT NULL OR team_worker IS NOT NULL;
CREATE INDEX "idx-store_path_content_producer-worker_registration"
    ON public.store_path_content_producer (worker_registration) WHERE worker_registration IS NOT NULL;
CREATE INDEX "idx-store_path_content_producer-team_worker"
    ON public.store_path_content_producer (team_worker) WHERE team_worker IS NOT NULL;
CREATE INDEX "idx-store_path_content_producer-cache"
    ON public.store_path_content_producer (cache) WHERE cache IS NOT NULL;
INSERT INTO public.store_path_content_producer (cached_path, kind)
    SELECT id, 3 FROM public.cached_path;

ALTER TABLE public.cached_path_signature ADD COLUMN store_path_hash text;
UPDATE public.cached_path_signature cps SET store_path_hash = cp.hash
    FROM public.cached_path cp WHERE cp.id = cps.cached_path;
ALTER TABLE public.cached_path_signature ALTER COLUMN store_path_hash SET NOT NULL;
CREATE FUNCTION public.copy_cached_path_hash_to_cached_path_signature_store_hash()
    RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    NEW.store_path_hash := (SELECT hash FROM public.cached_path WHERE id = NEW.cached_path);
    RETURN NEW;
END
$$;
CREATE TRIGGER "trg-cached_path_signature-store_path_hash"
    BEFORE INSERT OR UPDATE OF cached_path ON public.cached_path_signature
    FOR EACH ROW EXECUTE FUNCTION public.copy_cached_path_hash_to_cached_path_signature_store_hash();

UPDATE public.project_cache SET sign_outputs = false WHERE mode = 1;
ALTER TABLE public.project_cache
    ADD CONSTRAINT "chk-project_cache-read_only_does_not_sign" CHECK (mode <> 1 OR NOT sign_outputs);
"#;

const DOWN: &str = r#"
SET LOCAL lock_timeout = '5s';

ALTER TABLE public.project_cache DROP CONSTRAINT "chk-project_cache-read_only_does_not_sign";

DROP TRIGGER "trg-cached_path_signature-store_path_hash" ON public.cached_path_signature;
DROP FUNCTION public.copy_cached_path_hash_to_cached_path_signature_store_hash();
ALTER TABLE public.cached_path_signature DROP COLUMN store_path_hash;

DROP TABLE public.store_path_content_producer;

UPDATE public.derivation_build db
SET status = 0,
    substituted = false,
    attempt = 0,
    updated_at = (now() AT TIME ZONE 'UTC'),
    fetchable = false
WHERE db.status IN (3, 7)
  AND EXISTS (
      SELECT 1 FROM public.derivation_output o
      JOIN public.cached_path cp ON cp.hash = o.hash
      WHERE o.derivation = db.derivation
        AND o.name <> ALL (db.unneeded_outputs)
        AND cp.nar_object_storage_key <> cp.hash
        AND NOT EXISTS (
            SELECT 1 FROM public.cached_path legacy
            WHERE legacy.hash = o.hash AND legacy.nar_object_storage_key = legacy.hash
        )
  );
DELETE FROM public.cached_path WHERE nar_object_storage_key <> hash;
DELETE FROM public.cached_path cp
    WHERE EXISTS (
        SELECT 1 FROM public.cached_path older
        WHERE older.hash = cp.hash
          AND (older.created_at, older.id) < (cp.created_at, cp.id)
    );
DROP TRIGGER "trg-cached_path-nar_object_storage_key" ON public.cached_path;
DROP FUNCTION public.set_nar_object_storage_key_to_row_hash_when_null();
ALTER TABLE public.cached_path
    DROP COLUMN server_computed_hashes_and_sizes,
    DROP COLUMN nar_object_storage_key,
    ADD CONSTRAINT cached_path_hash_key UNIQUE (hash);
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
