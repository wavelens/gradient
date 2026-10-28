/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::exec_job::ExecJobStatus;
use gradient_types::{ExecJobId, MExecJob};
use sea_orm::{ConnectionTrait, DbErr, FromQueryResult, Value};

crate::sql! {
    EXEC_JOB_NODES = "SELECT * FROM exec_job \
         WHERE entry_point = (SELECT entry_point FROM exec_job WHERE id = $1) \
         ORDER BY node_index",
        params = [NewUuid];
    EXEC_JOB_NODE = "SELECT * FROM exec_job \
         WHERE entry_point = (SELECT entry_point FROM exec_job WHERE id = $1) \
         AND node_index = $2",
        params = [NewUuid, Int(1)];
}

pub async fn nodes_of<C: ConnectionTrait>(db: &C, job: ExecJobId) -> Result<Vec<MExecJob>, DbErr> {
    MExecJob::find_by_statement(EXEC_JOB_NODES.bind([Value::Uuid(Some(job.into_inner()))]))
        .all(db)
        .await
}

pub async fn node_of<C: ConnectionTrait>(
    db: &C,
    job: ExecJobId,
    node: u32,
) -> Result<Option<MExecJob>, DbErr> {
    let Ok(node_index) = i16::try_from(node) else {
        return Ok(None);
    };
    MExecJob::find_by_statement(EXEC_JOB_NODE.bind([
        Value::Uuid(Some(job.into_inner())),
        Value::SmallInt(Some(node_index)),
    ]))
    .one(db)
    .await
}

pub fn aggregate_status(statuses: &[ExecJobStatus]) -> ExecJobStatus {
    let open: Vec<ExecJobStatus> = statuses
        .iter()
        .copied()
        .filter(|status| !status.is_terminal())
        .collect();
    if !open.is_empty() {
        return if open.contains(&ExecJobStatus::Running) {
            ExecJobStatus::Running
        } else {
            lowest_open(&open)
        };
    }
    [ExecJobStatus::Aborted, ExecJobStatus::Failed]
        .into_iter()
        .find(|ended| statuses.contains(ended))
        .unwrap_or(ExecJobStatus::Completed)
}

fn lowest_open(open: &[ExecJobStatus]) -> ExecJobStatus {
    ExecJobStatus::OPEN
        .into_iter()
        .find(|status| open.contains(status))
        .unwrap_or(ExecJobStatus::Queued)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::{derivation, entry_point, exec_job};
    use gradient_fixtures::shape::shape;
    use gradient_types::EvaluationId;
    use sea_orm::{EntityTrait, IntoActiveModel};

    #[test]
    fn the_job_status_follows_its_nodes() {
        use ExecJobStatus::*;
        assert_eq!(aggregate_status(&[Completed, Running]), Running);
        assert_eq!(aggregate_status(&[Queued, Starting]), Queued);
        assert_eq!(aggregate_status(&[Completed, Failed]), Failed);
        assert_eq!(aggregate_status(&[Failed, Aborted]), Aborted);
        assert_eq!(aggregate_status(&[Completed, Completed]), Completed);
    }

    #[tokio::test]
    async fn nodes_of_selects_the_siblings_by_entry_point() {
        let evaluation = EvaluationId::now_v7();
        let db = shape()
            .evaluation(evaluation)
            .derivation("1b9p07z77phvv2hf6gm9f28syp39f1ag-train.drv")
            .entry_point("apps.x86_64-linux.train")
            .entry_point("apps.x86_64-linux.other")
            .into_database()
            .await;
        let program = db.rows::<derivation::Entity>().await[0].id;
        let points = db.rows::<entry_point::Entity>().await;
        let row = |entry_point, node_index| exec_job::Model {
            id: ExecJobId::now_v7(),
            evaluation,
            entry_point,
            program_derivation: program,
            node_index,
            requirement: serde_json::json!({}),
            spec: serde_json::json!({}),
            ..Default::default()
        };
        let (node1, node0, other) = (
            row(points[0].id, 1),
            row(points[0].id, 0),
            row(points[1].id, 0),
        );
        exec_job::Entity::insert_many(
            [&node1, &node0, &other].map(|job| job.clone().into_active_model()),
        )
        .exec(&db.connection())
        .await
        .expect("the node rows insert");

        let ids = |jobs: Vec<MExecJob>| jobs.into_iter().map(|job| job.id).collect::<Vec<_>>();
        let nodes = nodes_of(&db.connection(), node1.id).await.expect("nodes");
        assert_eq!(ids(nodes), vec![node0.id, node1.id]);
        let single = nodes_of(&db.connection(), other.id).await.expect("nodes");
        assert_eq!(ids(single), vec![other.id]);
        let unknown = nodes_of(&db.connection(), ExecJobId::now_v7()).await;
        assert!(unknown.expect("nodes").is_empty());
        let picked = node_of(&db.connection(), node0.id, 1).await.expect("node");
        assert_eq!(picked.map(|job| job.id), Some(node1.id));
        let missing = node_of(&db.connection(), node0.id, 2).await.expect("node");
        assert!(missing.is_none());
        db.finish().await;
    }
}
