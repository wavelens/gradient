/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::BTreeMap;

use chrono::NaiveDateTime;
use gradient_entity::cluster_job::{ClusterJobStatus, Entity as EClusterJob, Model as MClusterJob};
use gradient_entity::cluster_member::{Entity as EClusterMember, Model as MClusterMember};
use gradient_entity::exec_job::{self, ExecJobStatus};
use gradient_entity::ids::{ClusterJobId, ClusterMemberId};
use gradient_types::exec::{ExecDefaults, ExecSpec, exec_image_attr};
use gradient_types::{
    CExecJob, DerivationId, EExecJob, EntryPointId, EvaluationId, ExecJobId, MExecJob,
};
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ConnectionTrait, DatabaseTransaction, DbErr, EntityTrait, FromQueryResult, IntoActiveModel,
    TransactionTrait, Value,
};
use std::sync::LazyLock;

const CLUSTER_RETRY_BUDGET: i32 = 3;
const NODE_ROLE: &str = "node";

#[derive(Debug, FromQueryResult)]
struct Candidate {
    entry_point: EntryPointId,
    program_derivation: DerivationId,
    architecture: String,
    exec: serde_json::Value,
    image_derivation: Option<DerivationId>,
}

static CANDIDATES_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "SELECT e.id AS entry_point, e.derivation AS program_derivation, d.architecture, e.exec, \
         img.derivation AS image_derivation \
         FROM entry_point e \
         JOIN derivation d ON d.id = e.derivation \
         LEFT JOIN entry_point img ON img.evaluation = e.evaluation \
              AND img.eval = e.eval || '{image_suffix}' \
         WHERE e.evaluation = $1 AND e.exec IS NOT NULL \
         AND NOT EXISTS (SELECT 1 FROM exec_job r WHERE r.entry_point = e.id)",
        image_suffix = exec_image_attr(""),
    )
});

crate::sql_lazy! {
    EXEC_JOB_CANDIDATES = || CANDIDATES_SQL.as_str(),
        params = [EvaluationId];
}

pub async fn for_evaluation<C>(db: &C, evaluation: EvaluationId) -> Result<Vec<MExecJob>, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    let candidates = Candidate::find_by_statement(
        EXEC_JOB_CANDIDATES.bind([Value::Uuid(Some(evaluation.into_inner()))]),
    )
    .all(db)
    .await?;
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let rows: Vec<exec_job::ActiveModel> = candidates
        .into_iter()
        .flat_map(|candidate| created_jobs(evaluation, candidate))
        .map(IntoActiveModel::into_active_model)
        .collect();

    let txn = db.begin().await?;
    let created = EExecJob::insert_many(rows)
        .on_conflict(
            OnConflict::columns([CExecJob::EntryPoint, CExecJob::NodeIndex])
                .do_nothing()
                .to_owned(),
        )
        .exec_with_returning(&txn)
        .await?;
    insert_clusters(&txn, &created).await?;
    txn.commit().await?;
    Ok(created)
}

async fn insert_clusters<C: ConnectionTrait>(db: &C, created: &[MExecJob]) -> Result<(), DbErr> {
    let now = gradient_types::now();
    for nodes in nodes_by_entry_point(created).values() {
        let Some((cluster, members)) = cluster_rows(nodes, now) else {
            continue;
        };
        EClusterJob::insert(cluster.into_active_model())
            .exec(db)
            .await?;
        EClusterMember::insert_many(members.into_iter().map(IntoActiveModel::into_active_model))
            .exec(db)
            .await?;
    }
    Ok(())
}

fn nodes_by_entry_point(created: &[MExecJob]) -> BTreeMap<EntryPointId, Vec<MExecJob>> {
    let mut by_entry_point: BTreeMap<EntryPointId, Vec<MExecJob>> = BTreeMap::new();
    for job in created {
        by_entry_point
            .entry(job.entry_point)
            .or_default()
            .push(job.clone());
    }
    for nodes in by_entry_point.values_mut() {
        nodes.sort_by_key(|job| job.node_index);
    }
    by_entry_point
}

pub fn cluster_rows(
    nodes: &[MExecJob],
    now: NaiveDateTime,
) -> Option<(MClusterJob, Vec<MClusterMember>)> {
    if nodes.len() < 2 {
        return None;
    }
    let cluster = MClusterJob {
        id: ClusterJobId::now_v7(),
        status: ClusterJobStatus::Queued,
        same_zone: true,
        attempts: 0,
        retry_budget: CLUSTER_RETRY_BUDGET,
        created_at: now,
        updated_at: now,
        ..Default::default()
    };
    let members = nodes
        .iter()
        .map(|node| MClusterMember {
            id: ClusterMemberId::now_v7(),
            cluster_job: cluster.id,
            exec_job: Some(node.id),
            role: NODE_ROLE.to_owned(),
            primary: false,
            pin: None,
            ..Default::default()
        })
        .collect();
    Some((cluster, members))
}

fn created_jobs(evaluation: EvaluationId, candidate: Candidate) -> Vec<MExecJob> {
    let job = MExecJob {
        id: ExecJobId::now_v7(),
        evaluation,
        entry_point: candidate.entry_point,
        program_derivation: candidate.program_derivation,
        image_derivation: candidate.image_derivation,
        created_at: gradient_types::now(),
        ..Default::default()
    };
    match serde_json::from_value::<ExecSpec>(candidate.exec.clone()) {
        Ok(spec) => {
            let node = MExecJob {
                requirement: serde_json::to_value(
                    spec.requirement_on(&candidate.architecture, &ExecDefaults::default()),
                )
                .unwrap_or_default(),
                retries_left: i32::try_from(spec.retries).unwrap_or(i32::MAX),
                status: ExecJobStatus::Created,
                spec: candidate.exec,
                ..job
            };
            node_rows(node, spec.nodes)
        }
        Err(error) => {
            tracing::warn!(entry_point = %candidate.entry_point, %error, "exec spec does not decode");
            vec![failed_job(
                job,
                candidate.exec,
                "the exec spec could not be read",
            )]
        }
    }
}

fn node_rows(node: MExecJob, nodes: u32) -> Vec<MExecJob> {
    let Ok(count) = i16::try_from(nodes.max(1)) else {
        let spec = node.spec.clone();
        return vec![failed_job(
            node,
            spec,
            "an exec job has at most 32767 nodes",
        )];
    };
    (0..count)
        .map(|node_index| MExecJob {
            id: if node_index == 0 {
                node.id
            } else {
                ExecJobId::now_v7()
            },
            node_index,
            ..node.clone()
        })
        .collect()
}

fn failed_job(job: MExecJob, spec: serde_json::Value, message: &str) -> MExecJob {
    MExecJob {
        status: ExecJobStatus::Failed,
        message: Some(message.to_owned()),
        finished_at: Some(job.created_at),
        spec,
        ..job
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(exec: serde_json::Value) -> Candidate {
        Candidate {
            entry_point: EntryPointId::now_v7(),
            program_derivation: DerivationId::now_v7(),
            architecture: "x86_64-linux".into(),
            exec,
            image_derivation: None,
        }
    }

    fn node_row(node_index: i16) -> MExecJob {
        MExecJob {
            id: ExecJobId::now_v7(),
            node_index,
            ..Default::default()
        }
    }

    #[test]
    fn a_spec_that_does_not_decode_fails_its_job_instead_of_dropping_it() {
        let jobs = created_jobs(
            EvaluationId::now_v7(),
            candidate(serde_json::json!({ "nodes": "two" })),
        );
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].status, ExecJobStatus::Failed);
        assert_eq!(
            jobs[0].message.as_deref(),
            Some("the exec spec could not be read")
        );
        assert!(jobs[0].finished_at.is_some());
    }

    #[test]
    fn a_decoded_spec_waits_for_its_builds_with_its_retries() {
        let jobs = created_jobs(
            EvaluationId::now_v7(),
            candidate(
                serde_json::to_value(ExecSpec {
                    retries: 2,
                    ..ExecSpec::default()
                })
                .unwrap(),
            ),
        );
        assert_eq!(
            jobs.iter()
                .map(|job| (job.status, job.retries_left, job.node_index))
                .collect::<Vec<_>>(),
            vec![(ExecJobStatus::Created, 2, 0)]
        );
    }

    #[test]
    fn every_node_gets_its_own_row_of_the_same_entry_point() {
        let jobs = created_jobs(
            EvaluationId::now_v7(),
            candidate(
                serde_json::to_value(ExecSpec {
                    nodes: 3,
                    ..ExecSpec::default()
                })
                .unwrap(),
            ),
        );
        assert_eq!(
            jobs.iter().map(|job| job.node_index).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert!(
            jobs.iter()
                .all(|job| job.entry_point == jobs[0].entry_point)
        );
        assert_ne!(jobs[0].id, jobs[1].id);
    }

    #[test]
    fn two_nodes_get_a_same_zone_cluster_with_one_member_each() {
        let nodes = [node_row(0), node_row(1)];
        let (cluster, members) = cluster_rows(&nodes, gradient_types::now()).expect("cluster");
        assert!(cluster.same_zone);
        assert_eq!(
            (cluster.retry_budget, cluster.attempts, cluster.status),
            (3, 0, ClusterJobStatus::Queued)
        );
        assert_eq!(
            members.iter().map(|m| m.exec_job).collect::<Vec<_>>(),
            vec![Some(nodes[0].id), Some(nodes[1].id)]
        );
        assert!(members.iter().all(|m| m.role == "node"
            && !m.primary
            && m.pin.is_none()
            && m.cluster_job == cluster.id));
        assert!(cluster_rows(&nodes[..1], gradient_types::now()).is_none());
    }
}
