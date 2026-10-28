/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use gradient_entity::build::BuildStatus;
use gradient_entity::cluster_job::Model as MClusterJob;
use gradient_entity::cluster_job::{
    ClusterJobStatus, Column as CClusterJob, Entity as EClusterJob,
};
use gradient_entity::cluster_member::Model as MClusterMember;
use gradient_entity::cluster_member::{Column as CClusterMember, Entity as EClusterMember};
use gradient_entity::derivation_build::Entity as EDerivationBuild;
use gradient_entity::evaluation::{Entity as EEvaluation, EvaluationStatus};
use gradient_entity::exec_job::{Entity as EExecJob, ExecJobStatus};
use gradient_entity::ids::{
    ClusterJobId, ClusterMemberId, DerivationBuildId, EvaluationId, ExecJobId,
};
use gradient_types::{CDerivationBuild, CEvaluation, CExecJob};
use sea_orm::sea_query::{Cond, Expr, Query, SelectStatement};
use sea_orm::{
    ActiveEnum, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, ExprTrait, FromQueryResult,
    QueryFilter, Value,
};

pub async fn abort_dead_queued_clusters<C: ConnectionTrait>(
    db: &C,
) -> Result<Vec<ClusterJobId>, DbErr> {
    let aborted = EClusterJob::update_many()
        .col_expr(
            CClusterJob::Status,
            Expr::value(i16::from(ClusterJobStatus::Aborted)),
        )
        .col_expr(CClusterJob::UpdatedAt, Expr::value(gradient_types::now()))
        .filter(CClusterJob::Status.eq(ClusterJobStatus::Queued))
        .filter(Expr::exists(dead_member_of_cluster()))
        .exec_with_returning(db)
        .await?;

    Ok(aborted.into_iter().map(|c| c.id).collect())
}

crate::sql! {
    MEMBERSHIP = r#"
SELECT m.id AS member_id, m.cluster_job, m.evaluation, m.derivation_build, m.exec_job, m.role,
       m."primary", m.pin, m.eval_index, m.min_free_ram_mb, c.dissolve_after,
       c.status, c.same_zone, c.attempts, c.attempts_outside_budget, c.retry_budget,
       c.created_at, c.updated_at,
       (SELECT count(*) FROM cluster_member s WHERE s.cluster_job = m.cluster_job) AS member_count
FROM cluster_member m
JOIN cluster_job c ON c.id = m.cluster_job
WHERE m."evaluation" = ANY($1::uuid[]) OR m."derivation_build" = ANY($2::uuid[])
   OR m."exec_job" = ANY($3::uuid[])"#,
        params = [EvaluationIds(8), SharedBuildIds(8), NewUuids(8)];
}

#[derive(Debug, Clone)]
pub struct MemberOf {
    pub cluster: MClusterJob,
    pub member: MClusterMember,
    pub member_count: u32,
}

#[derive(FromQueryResult)]
struct MemberRow {
    member_id: ClusterMemberId,
    cluster_job: ClusterJobId,
    evaluation: Option<EvaluationId>,
    derivation_build: Option<DerivationBuildId>,
    exec_job: Option<ExecJobId>,
    role: String,
    primary: bool,
    pin: Option<String>,
    eval_index: Option<i16>,
    min_free_ram_mb: Option<i32>,
    dissolve_after: Option<NaiveDateTime>,
    status: ClusterJobStatus,
    same_zone: bool,
    attempts: i32,
    attempts_outside_budget: i32,
    retry_budget: i32,
    created_at: NaiveDateTime,
    updated_at: NaiveDateTime,
    member_count: i64,
}

impl From<MemberRow> for MemberOf {
    fn from(r: MemberRow) -> Self {
        Self {
            cluster: MClusterJob {
                id: r.cluster_job,
                status: r.status,
                same_zone: r.same_zone,
                attempts: r.attempts,
                retry_budget: r.retry_budget,
                created_at: r.created_at,
                updated_at: r.updated_at,
                dissolve_after: r.dissolve_after,
                attempts_outside_budget: r.attempts_outside_budget,
            },
            member: MClusterMember {
                id: r.member_id,
                cluster_job: r.cluster_job,
                evaluation: r.evaluation,
                derivation_build: r.derivation_build,
                role: r.role,
                primary: r.primary,
                pin: r.pin,
                eval_index: r.eval_index,
                min_free_ram_mb: r.min_free_ram_mb,
                exec_job: r.exec_job,
            },
            member_count: u32::try_from(r.member_count).unwrap_or(u32::MAX),
        }
    }
}

pub async fn cluster_membership<C: ConnectionTrait>(
    db: &C,
    evaluations: &[EvaluationId],
    shared_builds: &[DerivationBuildId],
    exec_jobs: &[ExecJobId],
) -> Result<Vec<MemberOf>, DbErr> {
    if evaluations.is_empty() && shared_builds.is_empty() && exec_jobs.is_empty() {
        return Ok(Vec::new());
    }

    let evaluations: Vec<uuid::Uuid> = evaluations.iter().map(|e| e.into_inner()).collect();
    let shared_builds: Vec<uuid::Uuid> = shared_builds.iter().map(|a| a.into_inner()).collect();
    let exec_jobs: Vec<uuid::Uuid> = exec_jobs.iter().map(|j| j.into_inner()).collect();
    let rows = MemberRow::find_by_statement(MEMBERSHIP.bind([
        Value::from(evaluations),
        Value::from(shared_builds),
        Value::from(exec_jobs),
    ]))
    .all(db)
    .await?;

    Ok(rows.into_iter().map(MemberOf::from).collect())
}

pub(crate) fn dead_member_of_cluster() -> SelectStatement {
    let dead_evaluation = Query::select()
        .expr(Expr::val(1))
        .from(EEvaluation)
        .and_where(
            Expr::col((EEvaluation, CEvaluation::Id))
                .equals((EClusterMember, CClusterMember::Evaluation)),
        )
        .and_where(
            Expr::col((EEvaluation, CEvaluation::Status))
                .is_in(EvaluationStatus::TERMINAL.map(|s| s.into_value())),
        )
        .to_owned();
    let dead_shared_build = Query::select()
        .expr(Expr::val(1))
        .from(EDerivationBuild)
        .and_where(
            Expr::col((EDerivationBuild, CDerivationBuild::Id))
                .equals((EClusterMember, CClusterMember::DerivationBuild)),
        )
        .and_where(
            Expr::col((EDerivationBuild, CDerivationBuild::Status)).is_in(
                BuildStatus::TERMINAL_SUCCESS
                    .into_iter()
                    .chain(BuildStatus::REQUEUEABLE)
                    .map(|s| s.into_value()),
            ),
        )
        .to_owned();
    let dead_exec_job = Query::select()
        .expr(Expr::val(1))
        .from(EExecJob)
        .and_where(
            Expr::col((EExecJob, CExecJob::Id)).equals((EClusterMember, CClusterMember::ExecJob)),
        )
        .and_where(
            Expr::col((EExecJob, CExecJob::Status)).is_in(
                [
                    ExecJobStatus::Completed,
                    ExecJobStatus::Failed,
                    ExecJobStatus::Aborted,
                ]
                .map(|s| s.into_value()),
            ),
        )
        .to_owned();

    Query::select()
        .expr(Expr::val(1))
        .from(EClusterMember)
        .and_where(
            Expr::col((EClusterMember, CClusterMember::ClusterJob))
                .equals((EClusterJob, CClusterJob::Id)),
        )
        .cond_where(
            Cond::any()
                .add(Expr::exists(dead_evaluation))
                .add(Expr::exists(dead_shared_build))
                .add(Expr::exists(dead_exec_job)),
        )
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::mock::{all_statements, empty_db, mock_db};

    use gradient_entity::ids::{ClusterMemberId, EvaluationId};

    fn membership_row(
        member: ClusterMemberId,
        cluster: ClusterJobId,
        evaluation: EvaluationId,
        count: i64,
    ) -> std::collections::BTreeMap<&'static str, sea_orm::Value> {
        let now = chrono::Utc::now().naive_utc();
        std::collections::BTreeMap::from([
            ("member_id", member.into_inner().into()),
            ("cluster_job", cluster.into_inner().into()),
            ("evaluation", Some(evaluation.into_inner()).into()),
            ("derivation_build", Option::<uuid::Uuid>::None.into()),
            ("exec_job", Option::<uuid::Uuid>::None.into()),
            ("role", "server".into()),
            ("primary", true.into()),
            ("pin", Option::<String>::None.into()),
            ("eval_index", Option::<i16>::None.into()),
            ("min_free_ram_mb", Option::<i32>::None.into()),
            (
                "dissolve_after",
                Option::<chrono::NaiveDateTime>::None.into(),
            ),
            ("status", i16::from(ClusterJobStatus::Queued).into()),
            ("same_zone", true.into()),
            ("attempts", 0i32.into()),
            ("attempts_outside_budget", 0i32.into()),
            ("retry_budget", 2i32.into()),
            ("created_at", now.into()),
            ("updated_at", now.into()),
            ("member_count", count.into()),
        ])
    }

    #[tokio::test]
    async fn membership_maps_each_member_to_its_cluster_and_size() {
        let (member, cluster, evaluation) = (
            ClusterMemberId::now_v7(),
            ClusterJobId::now_v7(),
            EvaluationId::now_v7(),
        );
        let (node, exec_job) = (ClusterMemberId::now_v7(), ExecJobId::now_v7());
        let mut exec_row = membership_row(node, cluster, evaluation, 3);
        exec_row.insert("evaluation", Option::<uuid::Uuid>::None.into());
        exec_row.insert("exec_job", Some(exec_job.into_inner()).into());
        let db = mock_db()
            .append_query_results([vec![
                membership_row(member, cluster, evaluation, 3),
                exec_row,
            ]])
            .into_connection();

        let found = cluster_membership(&db, &[evaluation], &[], &[exec_job])
            .await
            .expect("membership");

        assert_eq!(found.len(), 2);
        assert_eq!(found[1].member.exec_job, Some(exec_job));
        assert_eq!(found[1].member.evaluation, None);
        assert_eq!(found[0].member.id, member);
        assert_eq!(found[0].member.evaluation, Some(evaluation));
        assert_eq!(found[0].cluster.id, cluster);
        assert!(found[0].cluster.same_zone);
        assert_eq!(found[0].member_count, 3);
        let sql = &all_statements(db.into_transaction_log())[0].sql;
        assert!(sql.contains("\"evaluation\" = ANY($1::uuid[])"), "{sql}");
        assert!(
            sql.contains("\"derivation_build\" = ANY($2::uuid[])"),
            "{sql}"
        );
        assert!(sql.contains("\"exec_job\" = ANY($3::uuid[])"), "{sql}");
    }

    #[tokio::test]
    async fn membership_of_nothing_asks_nothing() {
        let db = empty_db();

        let found = cluster_membership(&db, &[], &[], &[])
            .await
            .expect("membership");

        assert!(found.is_empty());
        assert!(all_statements(db.into_transaction_log()).is_empty());
    }
}
