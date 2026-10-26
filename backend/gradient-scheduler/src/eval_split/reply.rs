/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{BTreeSet, HashMap};

use anyhow::Result;
use gradient_db::eval_shards::{self, NewShard};
use gradient_entity::cluster_member::{Column as CClusterMember, Entity as EClusterMember};
use gradient_entity::eval_shard::Model as MEvalShard;
use gradient_entity::eval_split::{
    Column as CEvalSplit, Entity as EEvalSplit, Model as MEvalSplit,
};
use gradient_types::ids::{EvalShardId, EvaluationId};
use gradient_wire::types::{
    BuildFailureKind, EvalShard, EvalShardOutcome, EvalShardSpec, FinishedShard,
};
use sea_orm::sea_query::Expr;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseTransaction, EntityTrait, PaginatorTrait, QueryFilter,
    TransactionTrait,
};
use tracing::{info, warn};

use super::planner::{ListedSet, PlannedShard, plan_shards};
use super::split_params;
use crate::Scheduler;
use crate::jobs::parse_eval_job_key;

pub const RETRY_AFTER_MS: u32 = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemberView {
    pub have_source: bool,
    pub is_primary: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SplitView {
    pub source: Option<(String, String)>,
    pub planned: bool,
}

impl From<&MEvalSplit> for SplitView {
    fn from(split: &MEvalSplit) -> Self {
        Self {
            source: split
                .source_store_path
                .clone()
                .zip(split.fingerprint.clone()),
            planned: split.planned_at.is_some(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NextStep {
    Wait,
    Source(String, String),
    Plan,
    Take,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceRecord {
    Stored,
    Skipped,
    FailPrimary(String),
}

pub fn next_step(split: &SplitView, member: &MemberView) -> NextStep {
    match (&split.source, member.have_source) {
        (None, false) => NextStep::Wait,
        (Some((store_path, fingerprint)), false) => {
            NextStep::Source(store_path.clone(), fingerprint.clone())
        }
        _ if !split.planned && member.is_primary => NextStep::Plan,
        _ if !split.planned => NextStep::Wait,
        _ => NextStep::Take,
    }
}

impl Scheduler {
    pub async fn eval_shard_request(
        &self,
        job_id: &str,
        have_source: bool,
        finished: Option<FinishedShard>,
    ) -> Result<EvalShardOutcome> {
        let Some((split, index)) = self.split_of(job_id).await? else {
            info!(%job_id, "shard request from a job that is not a split evaluation member");
            return Ok(EvalShardOutcome::Finished);
        };

        let member = MemberView {
            have_source,
            is_primary: index == 0,
        };
        let outcome = match next_step(&SplitView::from(&split), &member) {
            NextStep::Wait => wait(),
            NextStep::Source(store_path, fingerprint) => EvalShardOutcome::Source {
                store_path,
                fingerprint,
            },
            NextStep::Plan => EvalShardOutcome::Plan,
            NextStep::Take => self.take(split.evaluation, job_id, finished).await?,
        };

        Ok(outcome)
    }

    pub async fn eval_source_ready(
        &self,
        worker_id: &str,
        job_id: &str,
        store_path: String,
        fingerprint: String,
    ) -> Result<()> {
        let recorded = self
            .record_eval_source(job_id, store_path, fingerprint)
            .await?;
        if let SourceRecord::FailPrimary(error) = recorded {
            self.fail_split_primary(worker_id, job_id, &error).await?;
        }

        Ok(())
    }

    pub async fn record_eval_source(
        &self,
        job_id: &str,
        store_path: String,
        fingerprint: String,
    ) -> Result<SourceRecord> {
        let Some((split, 0)) = self.split_of(job_id).await? else {
            warn!(%job_id, "eval source report from a job that is not a split primary");
            return Ok(SourceRecord::Skipped);
        };

        let stored = EEvalSplit::update_many()
            .col_expr(CEvalSplit::SourceStorePath, Expr::value(store_path))
            .col_expr(CEvalSplit::Fingerprint, Expr::value(fingerprint))
            .filter(CEvalSplit::Evaluation.eq(split.evaluation))
            .exec(&self.db.worker_db)
            .await;
        let Err(e) = stored else {
            return Ok(SourceRecord::Stored);
        };
        if split.planned_at.is_some() {
            warn!(%job_id, error = %e, "storing the source of a planned split evaluation failed");
            return Ok(SourceRecord::Skipped);
        }

        Ok(SourceRecord::FailPrimary(format!(
            "storing the split evaluation source failed: {e}"
        )))
    }

    async fn fail_split_primary(&self, worker_id: &str, job_id: &str, error: &str) -> Result<()> {
        warn!(%job_id, %error, "failing the split primary so the attempt retries");
        self.handle_job_failed(
            worker_id,
            job_id,
            error,
            BuildFailureKind::Transient,
            &[],
            None,
        )
        .await?;
        self.abort_job(worker_id, job_id.to_owned(), error.to_owned())
            .await;

        Ok(())
    }

    pub async fn record_eval_plan(&self, job_id: &str, specs: Vec<EvalShardSpec>) -> Result<()> {
        let Some((split, 0)) = self.split_of(job_id).await? else {
            warn!(%job_id, "shard plan from a job that is not a split primary");
            return Ok(());
        };

        let db = &self.db.worker_db;
        let members = EClusterMember::find()
            .filter(CClusterMember::ClusterJob.eq(split.cluster_job))
            .count(db)
            .await?;
        let costs = match gradient_db::evaluations::task_of(db, split.evaluation).await? {
            Some(task) => gradient_db::eval_costs::load_eval_costs(db, task).await?,
            None => Default::default(),
        };
        let input = PlanInput::from(specs);
        let members = usize::try_from(members).unwrap_or(1).max(1);
        let planned = plan_shards(&costs, &input.listed, members, &split_params(members));
        let shards = input.shards(planned);

        let txn = db.begin().await?;
        if mark_planned(&txn, split.evaluation).await? {
            eval_shards::insert_shards(&txn, split.evaluation, &shards, None).await?;
        }
        txn.commit().await?;

        Ok(())
    }

    pub(crate) async fn split_of(&self, job_id: &str) -> Result<Option<(MEvalSplit, u16)>> {
        let Some((evaluation, index)) = parse_eval_job_key(job_id) else {
            return Ok(None);
        };
        let split = EEvalSplit::find_by_id(evaluation)
            .one(&self.db.worker_db)
            .await?;

        Ok(split.map(|split| (split, index)))
    }

    async fn take(
        &self,
        evaluation: EvaluationId,
        job_id: &str,
        finished: Option<FinishedShard>,
    ) -> Result<EvalShardOutcome> {
        let txn = self.db.worker_db.begin().await?;
        let stale = match finished {
            Some(finished) => {
                finish(&txn, job_id, finished).await?;
                Vec::new()
            }
            None => eval_shards::requeue_shards_of(&txn, job_id).await?,
        };

        let outcome = match eval_shards::take_shard(&txn, evaluation, job_id).await? {
            Some(shard) => EvalShardOutcome::Shard(given_shard(&txn, shard).await?),
            None if eval_shards::has_open_shards(&txn, evaluation).await? => wait(),
            None => EvalShardOutcome::Finished,
        };
        txn.commit().await?;
        self.record_failed_shards(&stale).await;

        Ok(outcome)
    }
}

async fn given_shard(txn: &DatabaseTransaction, shard: MEvalShard) -> Result<EvalShard> {
    let drv_paths = if shard.attempts > 0 {
        eval_shards::shard_drv_paths(
            txn,
            shard.evaluation,
            &shard.pattern,
            shard.children.as_deref(),
        )
        .await?
    } else {
        Vec::new()
    };

    Ok(EvalShard {
        id: shard.id.to_string(),
        pattern: shard.pattern,
        children: shard.children,
        drv_paths,
    })
}

fn wait() -> EvalShardOutcome {
    EvalShardOutcome::Wait {
        retry_after_ms: RETRY_AFTER_MS,
    }
}

async fn finish(txn: &DatabaseTransaction, job_id: &str, finished: FinishedShard) -> Result<()> {
    let Ok(shard) = finished.id.parse::<EvalShardId>() else {
        warn!(%job_id, id = %finished.id, "finished shard with an unparsable id");
        return Ok(());
    };

    let deferred: Vec<NewShard> = finished.deferred.into_iter().map(new_shard).collect();
    if !eval_shards::finish_shard(txn, shard, job_id, &deferred).await? {
        warn!(%job_id, %shard, "finished shard is not running for this job");
    }

    Ok(())
}

fn new_shard(spec: EvalShardSpec) -> NewShard {
    NewShard {
        pattern: spec.pattern,
        children: spec.children,
        predicted_ms: 0.0,
    }
}

async fn mark_planned<C: ConnectionTrait>(db: &C, evaluation: EvaluationId) -> Result<bool> {
    let marked = EEvalSplit::update_many()
        .col_expr(CEvalSplit::PlannedAt, Expr::value(gradient_types::now()))
        .filter(CEvalSplit::Evaluation.eq(evaluation))
        .filter(CEvalSplit::PlannedAt.is_null())
        .exec(db)
        .await?;

    Ok(marked.rows_affected == 1)
}

struct PlanInput {
    listed: Vec<ListedSet>,
    patterns: HashMap<Vec<String>, String>,
    whole: Vec<NewShard>,
}

impl From<Vec<EvalShardSpec>> for PlanInput {
    fn from(specs: Vec<EvalShardSpec>) -> Self {
        let mut children: HashMap<Vec<String>, BTreeSet<String>> = HashMap::new();
        let mut patterns: HashMap<Vec<String>, String> = HashMap::new();
        let mut order: Vec<Vec<String>> = Vec::new();
        let mut whole = Vec::new();
        for spec in specs {
            let Some(names) = spec.children.clone() else {
                whole.push(new_shard(spec));
                continue;
            };

            if !patterns.contains_key(&spec.set) {
                order.push(spec.set.clone());
                patterns.insert(spec.set.clone(), spec.pattern.clone());
            }

            children.entry(spec.set).or_default().extend(names);
        }

        let listed = order
            .into_iter()
            .map(|path| ListedSet {
                children: children
                    .remove(&path)
                    .unwrap_or_default()
                    .into_iter()
                    .collect(),
                path,
            })
            .collect();

        Self {
            listed,
            patterns,
            whole,
        }
    }
}

impl PlanInput {
    fn shards(self, planned: Vec<PlannedShard>) -> Vec<NewShard> {
        let patterns = self.patterns;
        planned
            .into_iter()
            .filter_map(|shard| {
                Some(NewShard {
                    pattern: patterns.get(&shard.set)?.clone(),
                    children: Some(shard.children),
                    predicted_ms: shard.predicted_ms,
                })
            })
            .chain(self.whole)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::eval_shard::{EvalShardState, Model as MEvalShard};
    use gradient_fixtures::mock::mock_db;
    use gradient_test_support::state::TestState;
    use gradient_types::ids::ClusterJobId;
    use sea_orm::DatabaseConnection;
    use std::collections::BTreeMap;

    fn scheduler(db: DatabaseConnection) -> Scheduler {
        Scheduler::new(TestState::new().worker_db(db).build())
    }

    fn split(evaluation: EvaluationId, source: bool, planned: bool) -> MEvalSplit {
        MEvalSplit {
            evaluation,
            cluster_job: ClusterJobId::now_v7(),
            source_store_path: source.then(|| "/nix/store/[hash140]-source".into()),
            fingerprint: source.then(|| "fp".into()),
            planned_at: planned.then(gradient_types::now),
        }
    }

    #[test]
    fn the_next_step_follows_source_and_plan() {
        let source = Some(("/nix/store/s".to_owned(), "fp".to_owned()));
        let companion = |have_source| MemberView {
            have_source,
            is_primary: false,
        };
        let primary = MemberView {
            have_source: true,
            is_primary: true,
        };
        let view = |source: Option<(String, String)>, planned| SplitView { source, planned };

        let rows = [
            (view(None, false), companion(false), NextStep::Wait),
            (
                view(source.clone(), false),
                companion(false),
                NextStep::Source("/nix/store/s".into(), "fp".into()),
            ),
            (view(source.clone(), false), primary, NextStep::Plan),
            (view(source.clone(), false), companion(true), NextStep::Wait),
            (view(source, true), companion(true), NextStep::Take),
        ];
        for (split, member, expected) in rows {
            assert_eq!(next_step(&split, &member), expected, "{split:?} {member:?}");
        }
    }

    async fn record_with_a_failing_store(planned: bool) -> SourceRecord {
        let evaluation = EvaluationId::now_v7();
        let db = mock_db()
            .append_query_results([vec![split(evaluation, false, planned)]])
            .append_exec_errors([sea_orm::DbErr::Custom("connection reset".into())])
            .into_connection();

        scheduler(db)
            .record_eval_source(
                &format!("eval:{evaluation}:0"),
                "/nix/store/hp2qck8z9dvkm59ilarq57kc0bppc065-source".into(),
                "fp".into(),
            )
            .await
            .expect("recorded")
    }

    #[tokio::test]
    async fn a_failed_source_store_fails_the_unplanned_primary() {
        let SourceRecord::FailPrimary(error) = record_with_a_failing_store(false).await else {
            panic!("an unplanned primary must fail");
        };

        assert!(error.contains("connection reset"), "{error}");
    }

    #[tokio::test]
    async fn a_planned_primary_is_not_failed_for_its_source() {
        assert_eq!(
            record_with_a_failing_store(true).await,
            SourceRecord::Skipped
        );
    }

    #[tokio::test]
    async fn a_companion_waits_until_the_plan_is_in() {
        let evaluation = EvaluationId::now_v7();
        let db = mock_db()
            .append_query_results([vec![split(evaluation, true, false)]])
            .into_connection();
        let scheduler = scheduler(db);

        let outcome = scheduler
            .eval_shard_request(&format!("eval:{evaluation}:1"), true, None)
            .await
            .expect("reply");

        assert_eq!(
            outcome,
            EvalShardOutcome::Wait {
                retry_after_ms: RETRY_AFTER_MS
            }
        );
    }

    #[tokio::test]
    async fn the_last_finished_shard_answers_finished() {
        let evaluation = EvaluationId::now_v7();
        let job = format!("eval:{evaluation}:1");
        let done = MEvalShard {
            id: EvalShardId::now_v7(),
            evaluation,
            seq: 1,
            pattern: "packages.*".into(),
            state: EvalShardState::Done,
            taken_job: Some(job.clone()),
            created_at: gradient_types::now(),
            ..Default::default()
        };
        let db = mock_db()
            .append_query_results([vec![split(evaluation, true, true)]])
            .append_query_results([vec![done.clone()]])
            .append_query_results([Vec::<MEvalShard>::new()])
            .append_query_results([Vec::<BTreeMap<&str, sea_orm::Value>>::new()])
            .into_connection();
        let scheduler = scheduler(db);

        let outcome = scheduler
            .eval_shard_request(
                &job,
                true,
                Some(FinishedShard {
                    id: done.id.to_string(),
                    deferred: Vec::new(),
                }),
            )
            .await
            .expect("reply");

        assert_eq!(outcome, EvalShardOutcome::Finished);
    }
}
