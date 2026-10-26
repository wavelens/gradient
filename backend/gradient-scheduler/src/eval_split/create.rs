/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use gradient_db::eval_costs::TaskEvalCosts;
use gradient_entity::cluster_job::{ClusterJobStatus, Entity as EClusterJob, Model as MClusterJob};
use gradient_entity::cluster_member::{
    ActiveModel as AClusterMember, Entity as EClusterMember, Model as MClusterMember,
};
use gradient_entity::dispatched_job::{
    Column as CDispatchedJob, DispatchedJobKind, Entity as EDispatchedJob,
};
use gradient_entity::eval_split::{Entity as EEvalSplit, Model as MEvalSplit};
use gradient_types::ids::{ClusterJobId, ClusterMemberId, DispatchedJobId, EvaluationId, TaskId};
use gradient_types::{CTask, ETask, MEvaluation};
use gradient_util::sync::Mutex;
use gradient_wire::types::{SPLIT_COMPANION_ROLE, SPLIT_PRIMARY_ROLE};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseTransaction, EntityTrait, IntoActiveModel, QueryFilter,
    QuerySelect, TransactionTrait,
};
use tracing::info;

use super::planner::choose_members;
use super::split_params;
use crate::Scheduler;
use crate::cluster::{Slot, SlotKind};

pub const MAX_SPLIT_MEMBERS: usize = 4;
pub const SPLIT_PLACE_TIMEOUT: Duration = Duration::from_secs(30);
pub const SPLIT_RETRY_BUDGET: i32 = 2;

#[derive(Clone, Debug, Default)]
pub struct SplitDecisions(Arc<Mutex<HashSet<EvaluationId>>>);

impl SplitDecisions {
    pub fn first_time(&self, evaluation: EvaluationId) -> bool {
        self.0.lock().insert(evaluation)
    }

    pub fn keep_only(&self, queued: &HashSet<EvaluationId>) {
        self.0
            .lock()
            .retain(|evaluation| queued.contains(evaluation));
    }
}

pub fn split_seats(slots: &[Slot]) -> usize {
    let workers: BTreeSet<&str> = slots
        .iter()
        .filter(|slot| slot.kind == SlotKind::Eval && !slot.caps.capabilities.federate)
        .map(|slot| slot.worker.as_str())
        .collect();

    workers.len().min(MAX_SPLIT_MEMBERS)
}

pub fn split_members(costs: &TaskEvalCosts, seats: usize) -> usize {
    if seats < 2 {
        return 1;
    }

    choose_members(costs, &split_params(seats))
}

impl Scheduler {
    pub async fn decide_split(&self, evaluation: &MEvaluation) -> Result<Option<ClusterJobId>> {
        if !self.split_decisions.first_time(evaluation.id) {
            return Ok(None);
        }

        let Some(task) = evaluation.task else {
            return Ok(None);
        };
        if !opted_in(&self.db.worker_db, task).await? {
            return Ok(None);
        }

        let seats = split_seats(&self.cluster_snapshot().await.slots);
        if seats < 2 {
            return Ok(None);
        }

        let costs = gradient_db::eval_costs::load_eval_costs(&self.db.worker_db, task).await?;
        let members = split_members(&costs, seats);
        if members < 2 {
            return Ok(None);
        }

        let created = create_split(
            &self.db.worker_db,
            evaluation.id,
            members,
            min_free_ram_mb(&costs),
        )
        .await?;
        if let Some(cluster) = created {
            info!(evaluation_id = %evaluation.id, %cluster, members, "evaluation split into a cluster job");
        }

        Ok(created)
    }
}

async fn opted_in<C: ConnectionTrait>(db: &C, task: TaskId) -> Result<bool> {
    let opted_in = ETask::find_by_id(task)
        .select_only()
        .column(CTask::MultiNodeEvaluation)
        .into_tuple::<bool>()
        .one(db)
        .await?;

    Ok(opted_in.unwrap_or(false))
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "the value is clamped to the i32 range first"
)]
fn min_free_ram_mb(costs: &TaskEvalCosts) -> Option<i32> {
    costs
        .sets
        .iter()
        .map(|set| set.max_peak_rss_mb)
        .reduce(f64::max)
        .map(|mb| mb.ceil().clamp(0.0, f64::from(i32::MAX)) as i32)
}

pub async fn create_split<C>(
    db: &C,
    evaluation: EvaluationId,
    members: usize,
    min_free_ram_mb: Option<i32>,
) -> Result<Option<ClusterJobId>>
where
    C: TransactionTrait<Transaction = DatabaseTransaction>,
{
    let txn = db.begin().await?;
    if is_dispatched(&txn, evaluation).await? {
        txn.rollback().await?;
        return Ok(None);
    }

    let cluster = cluster_row();
    let id = cluster.id;
    EClusterJob::insert(cluster.into_active_model())
        .exec_without_returning(&txn)
        .await?;
    EClusterMember::insert_many(member_rows(id, evaluation, members, min_free_ram_mb)?)
        .exec_without_returning(&txn)
        .await?;
    let split = MEvalSplit {
        evaluation,
        cluster_job: id,
        ..Default::default()
    };
    EEvalSplit::insert(split.into_active_model())
        .exec_without_returning(&txn)
        .await?;
    txn.commit().await?;

    Ok(Some(id))
}

async fn is_dispatched(txn: &DatabaseTransaction, evaluation: EvaluationId) -> Result<bool> {
    let open = EDispatchedJob::find()
        .select_only()
        .column(CDispatchedJob::Id)
        .filter(CDispatchedJob::EvaluationId.eq(evaluation))
        .filter(CDispatchedJob::Kind.eq(DispatchedJobKind::Eval))
        .filter(CDispatchedJob::FinishedAt.is_null())
        .into_tuple::<DispatchedJobId>()
        .one(txn)
        .await?;

    Ok(open.is_some())
}

fn cluster_row() -> MClusterJob {
    let now = gradient_types::now();
    let dissolve_after = chrono::TimeDelta::from_std(SPLIT_PLACE_TIMEOUT)
        .ok()
        .map(|timeout| now + timeout);

    MClusterJob {
        id: ClusterJobId::now_v7(),
        status: ClusterJobStatus::Queued,
        retry_budget: SPLIT_RETRY_BUDGET,
        created_at: now,
        updated_at: now,
        dissolve_after,
        ..Default::default()
    }
}

fn member_rows(
    cluster: ClusterJobId,
    evaluation: EvaluationId,
    members: usize,
    min_free_ram_mb: Option<i32>,
) -> Result<Vec<AClusterMember>> {
    (0..members)
        .map(|index| {
            let role = if index == 0 {
                SPLIT_PRIMARY_ROLE
            } else {
                SPLIT_COMPANION_ROLE
            };
            let member = MClusterMember {
                id: ClusterMemberId::now_v7(),
                cluster_job: cluster,
                evaluation: Some(evaluation),
                role: role.to_owned(),
                primary: index == 0,
                eval_index: Some(i16::try_from(index)?),
                min_free_ram_mb,
                ..Default::default()
            };

            Ok(member.into_active_model())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_db::eval_costs::{SetCost, SubtreeCost};
    use gradient_fixtures::mock::{all_statements, mock_db};
    use gradient_pool::WorkerCaps;
    use gradient_wire::types::GradientCapabilities;
    use std::collections::BTreeMap;

    fn slot(worker: &str, kind: SlotKind, federate: bool) -> Slot {
        Slot {
            worker: worker.into(),
            kind,
            zone: None,
            caps: WorkerCaps {
                capabilities: GradientCapabilities {
                    eval: true,
                    federate,
                    ..Default::default()
                },
                ..Default::default()
            },
            authorized: None,
        }
    }

    fn large_flake() -> TaskEvalCosts {
        let set = vec!["legacyPackages".to_owned(), "x86_64-linux".to_owned()];
        TaskEvalCosts {
            sets: vec![SetCost {
                path: set.clone(),
                startup_ms: Some(3000.0),
                peak_rss_mb: 2048.0,
                max_peak_rss_mb: 2048.0,
            }],
            subtrees: (0..400)
                .map(|index| SubtreeCost {
                    path: [set.clone(), vec![format!("p{index}")]].concat(),
                    attrs: 1.0,
                    ms_per_attr: 1000.0,
                })
                .collect(),
        }
    }

    #[test]
    fn no_split_without_history_or_with_one_idle_worker() {
        assert_eq!(split_members(&TaskEvalCosts::default(), 4), 1);
        assert_eq!(split_members(&large_flake(), 1), 1);
        assert_eq!(split_members(&large_flake(), 4), 4);
    }

    #[test]
    fn the_seat_ram_comes_from_the_largest_peak() {
        let mut costs = large_flake();
        costs.sets[0].peak_rss_mb = 600.0;
        costs.sets[0].max_peak_rss_mb = 900.4;

        assert_eq!(min_free_ram_mb(&costs), Some(901));
        assert_eq!(min_free_ram_mb(&TaskEvalCosts::default()), None);
    }

    #[test]
    fn federating_workers_are_not_counted_as_idle_seats() {
        let slots = [
            slot("w1", SlotKind::Eval, false),
            slot("w1", SlotKind::Build, false),
            slot("w2", SlotKind::Eval, true),
            slot("w3", SlotKind::Build, false),
            slot("w4", SlotKind::Eval, false),
        ];

        assert_eq!(split_seats(&slots), 2);
    }

    #[tokio::test]
    async fn no_split_when_the_task_has_not_opted_in() {
        let opted_out =
            BTreeMap::from([("multi_node_evaluation", sea_orm::Value::Bool(Some(false)))]);
        let db = mock_db()
            .append_query_results([vec![opted_out]])
            .into_connection();
        let scheduler = Scheduler::new(
            gradient_test_support::state::TestState::new()
                .worker_db(db)
                .build(),
        );
        let evaluation = MEvaluation {
            id: EvaluationId::now_v7(),
            task: Some(TaskId::now_v7()),
            ..Default::default()
        };

        let decided = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            scheduler.decide_split(&evaluation),
        )
        .await
        .expect("decided without asking the scheduler core for idle workers");

        assert_eq!(decided.expect("decision"), None);
    }

    #[tokio::test]
    async fn a_split_is_not_created_for_an_evaluation_already_dispatched() {
        let open = BTreeMap::from([(
            "id",
            sea_orm::Value::Uuid(Some(DispatchedJobId::now_v7().into_inner())),
        )]);
        let db = mock_db()
            .append_query_results([vec![open]])
            .into_connection();

        let created = create_split(&db, EvaluationId::now_v7(), 3, Some(1024))
            .await
            .expect("decision");

        assert_eq!(created, None);
        let statements = all_statements(db.into_transaction_log());
        assert!(
            !statements.iter().any(|s| s.sql.starts_with("INSERT")),
            "{statements:?}"
        );
        assert!(statements.iter().any(|s| s.sql == "ROLLBACK"));
        let probe = &statements[1].sql;
        assert!(probe.contains("\"evaluation_id\" = $1"), "{probe}");
        assert!(probe.contains("\"finished_at\" IS NULL"), "{probe}");
    }

    #[tokio::test]
    async fn a_split_creates_its_cluster_members_and_split_row_together() {
        let db = mock_db()
            .append_query_results([Vec::<BTreeMap<&str, sea_orm::Value>>::new()])
            .append_exec_results([exec(1), exec(3), exec(1)])
            .into_connection();

        let created = create_split(&db, EvaluationId::now_v7(), 3, Some(1024))
            .await
            .expect("decision");

        assert!(created.is_some());
        let statements = all_statements(db.into_transaction_log());
        let inserts: Vec<&str> = statements
            .iter()
            .filter(|s| s.sql.starts_with("INSERT"))
            .map(|s| s.sql.as_str())
            .collect();
        assert_eq!(inserts.len(), 3, "{inserts:?}");
        assert!(inserts[0].starts_with("INSERT INTO \"cluster_job\""));
        assert!(inserts[1].starts_with("INSERT INTO \"cluster_member\""));
        assert!(inserts[2].starts_with("INSERT INTO \"eval_split\""));
        let members = format!(
            "{:?}",
            statements
                .iter()
                .find(|s| s.sql == inserts[1])
                .map(|s| &s.values)
        );
        assert!(members.contains(SPLIT_PRIMARY_ROLE) && members.contains(SPLIT_COMPANION_ROLE));
        assert!(members.contains("SmallInt(Some(2))"), "{members}");
        assert_eq!(statements.last().map(|s| s.sql.as_str()), Some("COMMIT"));
    }

    fn exec(rows_affected: u64) -> sea_orm::MockExecResult {
        sea_orm::MockExecResult {
            last_insert_id: 0,
            rows_affected,
        }
    }
}
