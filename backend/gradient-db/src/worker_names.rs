/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::fetch_in_chunks;
use gradient_types::*;
use sea_orm::sea_query::Query;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbErr, EntityTrait, FromQueryResult, QueryFilter, QueryOrder,
    QuerySelect, Select,
};
use uuid::Uuid;

#[derive(FromQueryResult)]
pub struct WorkerName {
    pub worker_id: String,
    pub display_name: String,
}

fn registration_names_query(
    workers: Vec<String>,
    visible_projects: Option<&[Uuid]>,
) -> Select<EWorkerRegistration> {
    let query = EWorkerRegistration::find()
        .select_only()
        .column(CWorkerRegistration::WorkerId)
        .column(CWorkerRegistration::DisplayName)
        .filter(CWorkerRegistration::WorkerId.is_in(workers))
        .filter(CWorkerRegistration::DisplayName.ne(""))
        .order_by_desc(CWorkerRegistration::Active)
        .order_by_desc(CWorkerRegistration::CreatedAt);

    match visible_projects {
        Some(projects) => query.filter(CWorkerRegistration::PeerId.is_in(projects.to_vec())),
        None => query,
    }
}

fn team_worker_names_query(
    workers: Vec<String>,
    visible_projects: Option<&[Uuid]>,
) -> Select<ETeamWorker> {
    let query = ETeamWorker::find()
        .select_only()
        .column(CTeamWorker::WorkerId)
        .column(CTeamWorker::DisplayName)
        .filter(CTeamWorker::WorkerId.is_in(workers))
        .filter(CTeamWorker::DisplayName.ne(""));

    match visible_projects {
        Some(projects) => query.filter(
            CTeamWorker::Team.in_subquery(
                Query::select()
                    .column(CTeamProject::Team)
                    .from(gradient_entity::team_project::Entity)
                    .and_where(CTeamProject::IncludesWorkers.eq(true))
                    .and_where(CTeamProject::Project.is_in(projects.to_vec()))
                    .to_owned(),
            ),
        ),
        None => query,
    }
}

pub async fn of_registrations<C: ConnectionTrait>(
    db: &C,
    workers: &[String],
    visible_projects: Option<&[Uuid]>,
) -> Result<Vec<WorkerName>, DbErr> {
    fetch_in_chunks(workers, |chunk| {
        registration_names_query(chunk, visible_projects)
            .into_model()
            .all(db)
    })
    .await
}

pub async fn of_team_workers<C: ConnectionTrait>(
    db: &C,
    workers: &[String],
    visible_projects: Option<&[Uuid]>,
) -> Result<Vec<WorkerName>, DbErr> {
    fetch_in_chunks(workers, |chunk| {
        team_worker_names_query(chunk, visible_projects)
            .into_model()
            .all(db)
    })
    .await
}

pub(crate) fn worker_name_sql(worker_col: &str, project_filter: Option<&str>) -> String {
    let (registration_scope, team_scope) = project_filter
        .map(|list| {
            (
                format!(" AND wr.peer_id IN ({list})"),
                format!(
                    " AND tw.team IN (SELECT tp.team FROM team_project tp \
                     WHERE tp.includes_workers AND tp.project IN ({list}))"
                ),
            )
        })
        .unwrap_or_default();

    format!(
        "LEFT JOIN LATERAL ( \
           SELECT n.display_name FROM ( \
             SELECT wr.display_name, wr.active, wr.created_at FROM worker_registration wr \
             WHERE wr.worker_id = {worker_col}{registration_scope} \
             UNION ALL \
             SELECT tw.display_name, tw.active, tw.created_at FROM team_worker tw \
             WHERE tw.worker_id = {worker_col}{team_scope} \
           ) n WHERE n.display_name <> '' \
           ORDER BY n.active DESC, n.created_at DESC LIMIT 1 \
         ) wn ON true"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::DatabaseBackend;

    const SCOPE: &str = "'11111111-1111-1111-1111-111111111111'";

    #[test]
    fn worker_names_come_only_from_workers_the_caller_can_see() {
        use sea_orm::QueryTrait;
        let visible = [Uuid::now_v7()];
        let registrations = |projects: Option<&[Uuid]>| {
            registration_names_query(vec!["w1".into()], projects)
                .build(DatabaseBackend::Postgres)
                .to_string()
        };
        let team_workers = |projects: Option<&[Uuid]>| {
            team_worker_names_query(vec!["w1".into()], projects)
                .build(DatabaseBackend::Postgres)
                .to_string()
        };

        let scoped = registrations(Some(&visible));
        assert!(
            scoped.contains(&format!("\"peer_id\" IN ('{}')", visible[0])),
            "{scoped}"
        );
        assert!(scoped.contains("\"display_name\" <> ''"), "{scoped}");
        assert!(!registrations(None).contains("peer_id"));

        let scoped = team_workers(Some(&visible));
        assert!(scoped.contains("\"includes_workers\" = TRUE"), "{scoped}");
        assert!(
            scoped.contains(&format!("\"project\" IN ('{}')", visible[0])),
            "{scoped}"
        );
        assert!(scoped.contains("\"display_name\" <> ''"), "{scoped}");
        assert!(!team_workers(None).contains("team_project"));
    }

    #[test]
    fn worker_names_come_from_the_callers_own_registrations_and_team_workers() {
        let scoped = worker_name_sql("r.worker", Some(SCOPE));
        assert!(scoped.contains("wr.worker_id = r.worker"), "sql = {scoped}");
        assert!(scoped.contains("tw.worker_id = r.worker"), "sql = {scoped}");
        assert!(
            scoped.contains(&format!("wr.peer_id IN ({SCOPE})")),
            "sql = {scoped}"
        );
        assert!(
            scoped.contains(&format!(
                "WHERE tp.includes_workers AND tp.project IN ({SCOPE})"
            )),
            "sql = {scoped}"
        );
        let unscoped = worker_name_sql("r.worker", None);
        assert!(!unscoped.contains("peer_id") && !unscoped.contains("team_project"));
    }
}
