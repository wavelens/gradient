/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

// Direct membership rows now set a person's role. Team grants now take effect only for people who lack such a row. The code combined all rows before this change. The effect of this migration is removal of direct membership rows that grant less permission than the team grant for that person. No one can lose access. Down cannot restore these rows.
const UP: &str = r#"
ALTER TABLE public.project ADD COLUMN owner_team uuid
    REFERENCES public.team(id) ON UPDATE CASCADE ON DELETE SET NULL;
ALTER TABLE public.cache ADD COLUMN owner_team uuid
    REFERENCES public.team(id) ON UPDATE CASCADE ON DELETE SET NULL;
CREATE INDEX idx_project_owner_team ON public.project USING btree (owner_team);
CREATE INDEX idx_cache_owner_team ON public.cache USING btree (owner_team);

ALTER TABLE public.team_project ADD COLUMN grant_source smallint DEFAULT 0 NOT NULL;
UPDATE public.team_project SET grant_source = 2 WHERE managed;
ALTER TABLE public.team_project DROP COLUMN managed;
ALTER TABLE public.team_cache ADD COLUMN grant_source smallint DEFAULT 0 NOT NULL;
UPDATE public.team_cache SET grant_source = 2 WHERE managed;
ALTER TABLE public.team_cache DROP COLUMN managed;

WITH team_mask AS (
    SELECT tp.project, tu."user", bit_or(r.permission) AS permission
    FROM public.team_project tp
    JOIN public.team_user tu ON tu.team = tp.team
    JOIN public.role r ON r.id = tp.role
    WHERE tp.includes_users
    GROUP BY tp.project, tu."user"
)
DELETE FROM public.project_user pu
USING public.role r, team_mask t
WHERE r.id = pu.role AND t.project = pu.project AND t."user" = pu."user"
    AND r.permission & ~t.permission = 0 AND t.permission & ~r.permission <> 0;

WITH team_mask AS (
    SELECT tc.cache, tu."user", bit_or(r.permission) AS permission
    FROM public.team_cache tc
    JOIN public.team_user tu ON tu.team = tc.team
    JOIN public.cache_role r ON r.id = tc.role
    GROUP BY tc.cache, tu."user"
)
DELETE FROM public.cache_user cu
USING public.cache_role r, team_mask t
WHERE r.id = cu.role AND t.cache = cu.cache AND t."user" = cu."user"
    AND r.permission & ~t.permission = 0 AND t.permission & ~r.permission <> 0;

DROP VIEW public.project_access;
CREATE VIEW public.project_access AS
 SELECT pu.project, pu."user", pu.role, false AS via_team, NULL::uuid AS team
   FROM public.project_user pu
UNION ALL
 SELECT tp.project, tu."user", tp.role, true AS via_team, tp.team
   FROM public.team_project tp
   JOIN public.team_user tu ON tu.team = tp.team
  WHERE tp.includes_users AND NOT EXISTS (
    SELECT 1 FROM public.project_user pu WHERE pu.project = tp.project AND pu."user" = tu."user");

DROP VIEW public.cache_access;
CREATE VIEW public.cache_access AS
 SELECT cu.cache, cu."user", cu.role, false AS via_team, NULL::uuid AS team
   FROM public.cache_user cu
UNION ALL
 SELECT tc.cache, tu."user", tc.role, true AS via_team, tc.team
   FROM public.team_cache tc
   JOIN public.team_user tu ON tu.team = tc.team
  WHERE NOT EXISTS (
    SELECT 1 FROM public.cache_user cu WHERE cu.cache = tc.cache AND cu."user" = tu."user");
"#;

const DOWN: &str = r#"
DROP VIEW public.project_access;
CREATE VIEW public.project_access AS
 SELECT pu.project, pu."user", pu.role, false AS via_team
   FROM public.project_user pu
UNION ALL
 SELECT tp.project, tu."user", tp.role, true AS via_team
   FROM (public.team_project tp
     JOIN public.team_user tu ON ((tu.team = tp.team)))
  WHERE tp.includes_users;

DROP VIEW public.cache_access;
CREATE VIEW public.cache_access AS
 SELECT cu.cache, cu."user", cu.role, false AS via_team
   FROM public.cache_user cu
UNION ALL
 SELECT tc.cache, tu."user", tc.role, true AS via_team
   FROM (public.team_cache tc
     JOIN public.team_user tu ON ((tu.team = tc.team)));

ALTER TABLE public.team_project ADD COLUMN managed boolean DEFAULT false NOT NULL;
UPDATE public.team_project SET managed = true WHERE grant_source = 2;
ALTER TABLE public.team_project DROP COLUMN grant_source;
ALTER TABLE public.team_cache ADD COLUMN managed boolean DEFAULT false NOT NULL;
UPDATE public.team_cache SET managed = true WHERE grant_source = 2;
ALTER TABLE public.team_cache DROP COLUMN grant_source;

ALTER TABLE public.cache DROP COLUMN owner_team;
ALTER TABLE public.project DROP COLUMN owner_team;
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
