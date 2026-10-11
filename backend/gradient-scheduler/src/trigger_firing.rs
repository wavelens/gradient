/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_db::lookup::{by_id, by_ids};
use std::time::Duration;

use chrono::{NaiveDateTime, Utc};

use gradient_types::TaskTriggerId;

const POLLING_JITTER_PCT: u32 = 10;

const HEAD_RESOLVE_BUDGET: std::time::Duration = std::time::Duration::from_secs(30);

pub(crate) fn polling_due(
    trigger_id: TaskTriggerId,
    last_fired_at: Option<NaiveDateTime>,
    interval_secs: u32,
    now: NaiveDateTime,
) -> bool {
    match last_fired_at {
        None => true,
        Some(t) => {
            let jitter = polling_jitter_secs(trigger_id, t, interval_secs);
            (now - t).num_seconds() >= interval_secs as i64 + jitter as i64
        }
    }
}

fn polling_jitter_secs(
    trigger_id: TaskTriggerId,
    last_fired_at: NaiveDateTime,
    interval_secs: u32,
) -> u32 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let max = interval_secs / (100 / POLLING_JITTER_PCT);
    if max == 0 {
        return 0;
    }
    let mut hasher = DefaultHasher::new();
    trigger_id.hash(&mut hasher);
    last_fired_at.hash(&mut hasher);
    (hasher.finish() % (max as u64 + 1)) as u32
}

pub(crate) fn cron_due(
    cron_expr: &str,
    last_fired_at: Option<NaiveDateTime>,
    now: NaiveDateTime,
) -> bool {
    use cron::Schedule;
    use std::str::FromStr;
    let Ok(sched) = Schedule::from_str(cron_expr) else {
        return false;
    };
    let after = last_fired_at.unwrap_or(now - chrono::Duration::days(1));
    let after_utc = chrono::DateTime::<Utc>::from_naive_utc_and_offset(after, Utc);
    let now_utc = chrono::DateTime::<Utc>::from_naive_utc_and_offset(now, Utc);
    sched
        .after(&after_utc)
        .next()
        .map(|next| next <= now_utc)
        .unwrap_or(false)
}

use gradient_ci::{ApplyInput, ApplyOutcome, apply_trigger, trigger::maybe_trigger_input_update};
use gradient_db::deliveries::trigger_fire::{TriggerFire, enqueue_trigger_fires};
use gradient_db::{DbContext, WorkerDb};
use gradient_entity::task_trigger as ept;
use gradient_sources::{check_task_updates, get_commit_info};
use gradient_types::triggers::{TriggerConfig, TriggerType};
use gradient_types::*;
use gradient_util::supervision::{Pass, PassError, RunsOn};
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter, QueryOrder};
use tracing::{debug, error, info, warn};

use super::Scheduler;

pub(crate) struct TriggerDispatch {
    pub(crate) db: DbContext,
}

impl Pass for TriggerDispatch {
    const NAME: &'static str = "trigger-dispatch";
    const RUNS_ON: RunsOn = RunsOn::EveryInstance;
    const BUDGET: Duration = crate::loops::ASSIGN_BUDGET;

    fn period(&self) -> Duration {
        crate::loops::ASSIGN_TICK
    }

    async fn run(&self) -> Result<(), PassError> {
        fire_once(&self.db).await.map_err(Into::into)
    }
}

pub(crate) async fn fire_once(db: &DbContext) -> anyhow::Result<()> {
    let now = gradient_types::now();

    let triggers = active_triggers(&db.worker_db).await?;
    if triggers.is_empty() {
        return Ok(());
    }
    let tasks = tasks_of(&db.worker_db, &triggers).await?;

    let due: Vec<TriggerFire> = triggers
        .iter()
        .filter(|trig| tasks.get(&trig.task).is_some_and(|task| task.active))
        .filter_map(|trig| due_fire(trig, now))
        .collect();
    if due.is_empty() {
        return Ok(());
    }

    enqueue_trigger_fires(&db.worker_db, &due).await?;
    db.wake.delivery.notify_one();
    Ok(())
}

pub async fn fire(scheduler: &Scheduler, fire: &TriggerFire) -> anyhow::Result<()> {
    let Some((trig, task, cfg)) = load_pending_firing(&scheduler.db.worker_db, fire).await? else {
        return Ok(());
    };
    let now = gradient_types::now();

    let sources = &scheduler.db;
    let branch = polling_branch(&cfg);
    let resolve = check_task_updates(sources, &task, branch.as_deref());
    if let Some((has_update, commit_hash)) = resolve_head(resolve, &task.name).await {
        apply_resolved_head(scheduler, &trig, &task, &cfg, has_update, commit_hash).await;
    }

    stamp_last_fired(&scheduler.db.worker_db, &trig, now).await
}

async fn active_triggers(db: &WorkerDb) -> Result<Vec<ept::Model>, sea_orm::DbErr> {
    ept::Entity::find()
        .filter(ept::Column::Active.eq(true))
        .filter(
            Condition::any()
                .add(ept::Column::TriggerType.eq(i16::from(TriggerType::Polling)))
                .add(ept::Column::TriggerType.eq(i16::from(TriggerType::Time))),
        )
        .order_by_asc(ept::Column::LastFiredAt)
        .all(db)
        .await
}

async fn tasks_of(
    db: &WorkerDb,
    triggers: &[ept::Model],
) -> Result<std::collections::HashMap<TaskId, MTask>, sea_orm::DbErr> {
    let task_ids: Vec<_> = triggers
        .iter()
        .map(|t| t.task)
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    by_ids::<ETask, _>(db, &task_ids).await
}

fn parse_config(trig: &ept::Model) -> Option<TriggerConfig> {
    match TriggerConfig::parse_row(trig.trigger_type, &trig.config) {
        Ok(cfg) => Some(cfg),
        Err(e) => {
            warn!(trigger_id = %trig.id, error = %e, "skipping trigger with invalid config");
            None
        }
    }
}

fn due_fire(trig: &ept::Model, now: NaiveDateTime) -> Option<TriggerFire> {
    let due = match parse_config(trig)? {
        TriggerConfig::Polling { interval_secs, .. } => {
            polling_due(trig.id, trig.last_fired_at, interval_secs, now)
        }
        TriggerConfig::Time { cron } => cron_due(&cron, trig.last_fired_at, now),
        _ => false,
    };

    due.then_some(TriggerFire {
        trigger: trig.id,
        last_fired_at: trig.last_fired_at,
    })
}

async fn load_pending_firing(
    db: &WorkerDb,
    fire: &TriggerFire,
) -> Result<Option<(ept::Model, MTask, TriggerConfig)>, sea_orm::DbErr> {
    let Some(trig) = by_id::<ept::Entity, _>(db, fire.trigger).await? else {
        return Ok(None);
    };
    if !trig.active || trig.last_fired_at != fire.last_fired_at {
        return Ok(None);
    }
    let Some(task) = by_id::<ETask, _>(db, trig.task).await? else {
        return Ok(None);
    };
    if !task.active {
        return Ok(None);
    }

    Ok(parse_config(&trig).map(|cfg| (trig, task, cfg)))
}

fn polling_branch(cfg: &TriggerConfig) -> Option<String> {
    match cfg {
        TriggerConfig::Polling { branch, .. } => branch.clone(),
        _ => None,
    }
}

async fn apply_resolved_head(
    scheduler: &Scheduler,
    trig: &ept::Model,
    task: &MTask,
    cfg: &TriggerConfig,
    has_update: bool,
    commit_hash: Vec<u8>,
) {
    if let Err(e) = maybe_trigger_input_update(
        &scheduler.db.worker_db,
        task,
        commit_hash.clone(),
        Some(trig.id),
    )
    .await
    {
        warn!(error = %e, trigger_id = %trig.id, "input_update trigger failed");
    }

    if has_update || matches!(cfg, TriggerConfig::Time { .. }) {
        apply_to_task(scheduler, trig, task, cfg, commit_hash).await;
    }
}

async fn apply_to_task(
    scheduler: &Scheduler,
    trig: &ept::Model,
    task: &MTask,
    cfg: &TriggerConfig,
    commit_hash: Vec<u8>,
) {
    let (msg, author) = commit_metadata(&scheduler.db, task, &commit_hash).await;
    let outcome = apply_trigger(
        &scheduler.db.worker_db,
        task,
        ApplyInput {
            trigger_id: trig.id,
            trigger_type: cfg.trigger_type(),
            commit_hash,
            commit_message: Some(msg),
            author_name: Some(author),
            manual: false,
            gate_approval: None,
            repository_override: None,
            wildcard_override: None,
            source_comment: None,
            instance_max_storage_gb: scheduler.db.config.cache.max_storage_gb,
        },
    )
    .await;

    match outcome {
        Ok(ApplyOutcome::Created {
            evaluation,
            aborted_evaluation,
            hard_abort,
        }) => {
            if let Some(aborted_id) = aborted_evaluation {
                cancel_aborted(scheduler, aborted_id, hard_abort).await;
            }
            info!(task = %task.name, trigger_id = %trig.id, evaluation_id = %evaluation.id, "trigger created evaluation");
            scheduler.db.record_evaluation_created(&evaluation).await;
        }
        Ok(other) => {
            debug!(task = %task.name, trigger_id = %trig.id, ?other, "trigger applied without creating eval");
        }
        Err(e) => {
            error!(error = %e, trigger_id = %trig.id, "trigger application failed");
        }
    }
}

async fn cancel_aborted(scheduler: &Scheduler, aborted_id: EvaluationId, hard_abort: bool) {
    let shared_builds = if hard_abort {
        scheduler.abort_evaluation_shared_builds(aborted_id).await
    } else {
        Vec::new()
    };

    scheduler
        .cancel_evaluation_jobs(aborted_id, &shared_builds)
        .await;
}

async fn commit_metadata(
    sources: &DbContext,
    task: &MTask,
    commit_hash: &[u8],
) -> (String, String) {
    match tokio::time::timeout(
        HEAD_RESOLVE_BUDGET,
        get_commit_info(sources, task, commit_hash),
    )
    .await
    {
        Ok(Ok(commit)) => (commit.message, commit.author_name),
        _ => (String::new(), String::new()),
    }
}

async fn resolve_head<F, T, E>(resolve: F, task: &str) -> Option<T>
where
    F: std::future::Future<Output = Result<T, E>>,
    E: std::fmt::Display,
{
    match tokio::time::timeout(HEAD_RESOLVE_BUDGET, resolve).await {
        Ok(Ok(v)) => Some(v),
        Ok(Err(e)) => {
            warn!(error = %e, %task, "trigger commit resolution failed");
            None
        }
        Err(_) => {
            warn!(%task, budget_secs = HEAD_RESOLVE_BUDGET.as_secs(), "trigger commit resolution timed out");
            None
        }
    }
}

async fn stamp_last_fired(
    db: &WorkerDb,
    trig: &ept::Model,
    now: NaiveDateTime,
) -> anyhow::Result<()> {
    let unchanged = match trig.last_fired_at {
        Some(at) => ept::Column::LastFiredAt.eq(at),
        None => ept::Column::LastFiredAt.is_null(),
    };
    ept::Entity::update_many()
        .col_expr(ept::Column::LastFiredAt, Expr::value(now))
        .col_expr(ept::Column::UpdatedAt, Expr::value(now))
        .filter(ept::Column::Id.eq(trig.id))
        .filter(unchanged)
        .exec(db)
        .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::mock::{all_statements, exec_ok, mock_db};
    use gradient_fixtures::rows;
    use gradient_test_support::state::TestState;

    fn dt(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").unwrap()
    }

    fn tid() -> TaskTriggerId {
        TaskTriggerId::now_v7()
    }

    #[tokio::test(start_paused = true)]
    async fn a_hanging_remote_gives_up_its_slot_instead_of_the_pass() {
        let hang = std::future::pending::<Result<(bool, Vec<u8>), std::io::Error>>();
        assert!(
            resolve_head(hang, "never-answers").await.is_none(),
            "the pass moves on to the next trigger"
        );

        let ok = std::future::ready(Ok::<_, std::io::Error>((true, vec![1u8])));
        assert_eq!(resolve_head(ok, "answers").await, Some((true, vec![1u8])));
    }

    #[test]
    fn polling_no_prior_fires_now() {
        assert!(polling_due(tid(), None, 60, dt("2026-05-06 10:00:00")));
    }

    #[test]
    fn polling_under_interval_does_not_fire() {
        assert!(!polling_due(
            tid(),
            Some(dt("2026-05-06 10:00:00")),
            60,
            dt("2026-05-06 10:00:30")
        ));
    }

    #[test]
    fn polling_well_past_interval_plus_max_jitter_fires() {
        let id = tid();
        assert!(polling_due(
            id,
            Some(dt("2026-05-06 10:00:00")),
            60,
            dt("2026-05-06 10:01:30")
        ));
    }

    #[test]
    fn polling_jitter_within_ten_percent_bound() {
        let last = dt("2026-05-06 10:00:00");
        let interval = 600;
        let max = interval / 10;
        for _ in 0..1000 {
            let j = polling_jitter_secs(tid(), last, interval);
            assert!(j <= max, "jitter {j} exceeded 10% bound {max}");
        }
    }

    #[test]
    fn polling_jitter_is_deterministic_per_cycle() {
        let id = tid();
        let last = dt("2026-05-06 10:00:00");
        let a = polling_jitter_secs(id, last, 600);
        let b = polling_jitter_secs(id, last, 600);
        assert_eq!(
            a, b,
            "same (trigger_id, last_fired_at) must yield same jitter"
        );
    }

    #[test]
    fn polling_jitter_changes_between_cycles() {
        let id = tid();
        let a = polling_jitter_secs(id, dt("2026-05-06 10:00:00"), 600);
        let b = polling_jitter_secs(id, dt("2026-05-06 10:11:00"), 600);
        let c = polling_jitter_secs(id, dt("2026-05-06 10:22:00"), 600);
        assert!(a != b || a != c, "jitter must vary across cycles");
    }

    #[test]
    fn polling_jitter_zero_when_interval_below_threshold() {
        assert_eq!(polling_jitter_secs(tid(), dt("2026-05-06 10:00:00"), 9), 0);
    }

    #[test]
    fn polling_does_not_fire_before_interval_plus_jitter() {
        let id = TaskTriggerId::now_v7();
        let last = dt("2026-05-06 10:00:00");
        let jitter = polling_jitter_secs(id, last, 100);
        if jitter == 0 {
            return;
        }
        let just_before = last + chrono::Duration::seconds(100 + jitter as i64 - 1);
        let exactly_at = last + chrono::Duration::seconds(100 + jitter as i64);
        assert!(!polling_due(id, Some(last), 100, just_before));
        assert!(polling_due(id, Some(last), 100, exactly_at));
    }

    #[test]
    fn cron_every_minute_fires_after_minute_boundary() {
        let last = dt("2026-05-06 10:00:30");
        let now = dt("2026-05-06 10:01:05");
        assert!(cron_due("0 * * * * *", Some(last), now));
    }

    #[test]
    fn cron_does_not_fire_before_next_boundary() {
        let last = dt("2026-05-06 10:01:00");
        let now = dt("2026-05-06 10:01:30");
        assert!(!cron_due("0 * * * * *", Some(last), now));
    }

    #[test]
    fn cron_invalid_does_not_fire() {
        assert!(!cron_due("garbage", None, dt("2026-05-06 10:00:00")));
    }

    #[test]
    fn cron_no_prior_fires_when_due() {
        let now = dt("2026-05-06 03:00:00");
        assert!(cron_due("0 0 2 * * *", None, now));
    }

    fn polling_trigger(last_fired_at: Option<NaiveDateTime>) -> ept::Model {
        ept::Model {
            id: TaskTriggerId::now_v7(),
            task: rows::task().id,
            config: serde_json::json!({ "interval_secs": 60 }),
            active: true,
            last_fired_at,
            ..Default::default()
        }
    }

    fn scheduler_over(db: sea_orm::DatabaseConnection) -> Scheduler {
        Scheduler::new(TestState::new().worker_db(db).build())
    }

    fn executed(db: sea_orm::DatabaseConnection) -> Vec<String> {
        all_statements(db.into_transaction_log())
            .into_iter()
            .map(|s| s.sql)
            .collect()
    }

    #[tokio::test]
    async fn a_firing_another_instance_already_handled_applies_nothing() {
        let handled = polling_trigger(Some(dt("2026-05-06 10:05:00")));
        let db = mock_db()
            .append_query_results([vec![handled.clone()]])
            .into_connection();
        let log = db.clone();
        let outdated = TriggerFire {
            trigger: handled.id,
            last_fired_at: Some(dt("2026-05-06 10:00:00")),
        };

        fire(&scheduler_over(db), &outdated).await.unwrap();

        let sql = executed(log);
        assert!(!sql.iter().any(|s| s.contains("INSERT INTO \"evaluation\"")));
        assert!(!sql.iter().any(|s| s.contains("UPDATE \"task_trigger\"")));
    }

    #[tokio::test]
    async fn the_pass_queues_a_due_trigger_and_stamps_nothing() {
        let due = polling_trigger(None);
        let db = mock_db()
            .append_query_results([vec![due]])
            .append_query_results([vec![rows::task()]])
            .append_exec_results([exec_ok()])
            .into_connection();
        let log = db.clone();

        fire_once(&scheduler_over(db).db).await.unwrap();

        let sql = executed(log);
        assert!(
            sql.iter()
                .any(|s| s.contains("INSERT INTO pending_delivery"))
        );
        assert!(!sql.iter().any(|s| s.contains("UPDATE \"task_trigger\"")));
    }
}
