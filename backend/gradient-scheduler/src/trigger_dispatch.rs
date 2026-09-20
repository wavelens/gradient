/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Per-trigger scheduling: decides whether a trigger is due to fire and
//! drives the unified dispatch loop (replaces the legacy poll loop).
//!
//! `polling_due` and `cron_due` are pure helpers testable without time
//! manipulation. `trigger_dispatch_loop` / `dispatch_once` are the live
//! DB-driven loop that replaced the legacy `task_poll_loop`.

use chrono::{NaiveDateTime, Utc};

use gradient_types::TaskTriggerId;

/// Upper bound of the jitter added to each polling cycle, expressed as a
/// percentage of `interval_secs`. Spreads concurrently-created triggers out
/// over time so they don't all hit the upstream at the same moment.
const POLLING_JITTER_PCT: u32 = 10;

/// How long one trigger may spend resolving its repository's HEAD before the
/// pass gives up on it and moves to the next.
///
/// The resolution is a network round-trip to a forge, and the pass walks the
/// triggers in one sequence under a shared budget: unbounded, a single
/// unreachable remote consumes the whole budget, the pass is cancelled before it
/// reaches `update_last_fired`, and every trigger behind that one stops firing
/// for good. Bounding it here plus the ordering in [`dispatch_once`] means a
/// stuck remote costs one slot per pass and nothing else.
const HEAD_RESOLVE_BUDGET: std::time::Duration = std::time::Duration::from_secs(30);

/// `true` if a polling trigger with the given `interval_secs` should fire
/// at `now`, given that it last fired at `last_fired_at`. A trigger that has
/// never fired (`None`) is always due.
///
/// The effective wait is `interval_secs + jitter`, where jitter is in
/// `[0, interval_secs * POLLING_JITTER_PCT / 100]` and is derived
/// deterministically from `(trigger_id, last_fired_at)`. Keeping the jitter
/// stable within a cycle means the firing decision doesn't oscillate; a fresh
/// value is drawn after each fire because `last_fired_at` advances.
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

/// Deterministic jitter in `[0, interval_secs * POLLING_JITTER_PCT / 100]`
/// for a single polling cycle.
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

/// `true` if a six-field cron expression (sec min hour dom mon dow) has a
/// firing time strictly after `last_fired_at` and at or before `now`.
/// Invalid expressions return `false` (we'd rather skip than crash).
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

use std::sync::Arc;

use gradient_ci::{ApplyInput, ApplyOutcome, apply_trigger, trigger::maybe_trigger_input_update};
use gradient_core::ServerState;
use gradient_entity::task_trigger as ept;
use gradient_graph::Transition;
use gradient_sources::{check_task_updates, get_commit_info};
use gradient_types::triggers::{TriggerConfig, TriggerType};
use gradient_types::*;
use sea_orm::{
    ActiveModelTrait as _, ColumnTrait, Condition, EntityTrait, QueryFilter, QueryOrder,
};
use tracing::{debug, error, info, warn};

use super::Scheduler;

/// One pass through all active polling+time triggers. Public for tests.
pub(crate) async fn dispatch_once(scheduler: &Scheduler) -> anyhow::Result<()> {
    let state = &scheduler.state;
    let now = gradient_types::now();

    // Least-recently-fired first, so a trigger a slow remote cost a slot last pass
    // is served before the one that cost it.
    let triggers = ept::Entity::find()
        .filter(ept::Column::Active.eq(true))
        .filter(
            Condition::any()
                .add(ept::Column::TriggerType.eq(i16::from(TriggerType::Polling)))
                .add(ept::Column::TriggerType.eq(i16::from(TriggerType::Time))),
        )
        .order_by_asc(ept::Column::LastFiredAt)
        .all(&state.worker_db)
        .await?;
    if triggers.is_empty() {
        return Ok(());
    }

    let task_ids: Vec<_> = triggers
        .iter()
        .map(|t| t.task)
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    let db = &state.worker_db;
    let tasks: std::collections::HashMap<_, _> =
        gradient_db::fetch_in_chunks(&task_ids, |chunk| async move {
            ETask::find().filter(CTask::Id.is_in(chunk)).all(db).await
        })
        .await?
        .into_iter()
        .map(|p| (p.id, p))
        .collect();

    for trig in triggers {
        let Some(task) = tasks.get(&trig.task) else {
            continue;
        };
        if !task.active {
            continue;
        }

        let cfg = match TriggerConfig::parse_row(trig.trigger_type, &trig.config) {
            Ok(c) => c,
            Err(e) => {
                warn!(trigger_id = %trig.id, error = %e, "skipping trigger with invalid config");
                continue;
            }
        };

        let is_time = matches!(cfg, TriggerConfig::Time { .. });
        let branch_for_check: Option<String> = match &cfg {
            TriggerConfig::Polling { branch, .. } => branch.clone(),
            _ => None,
        };
        let due = match &cfg {
            TriggerConfig::Polling { interval_secs, .. } => {
                polling_due(trig.id, trig.last_fired_at, *interval_secs, now)
            }
            TriggerConfig::Time { cron } => cron_due(cron, trig.last_fired_at, now),
            _ => false,
        };
        if !due {
            continue;
        }

        // Resolve current HEAD. Polling reports whether it advanced; time
        // triggers fire on whatever HEAD currently is.
        let sources = state.db();
        let resolve = check_task_updates(&sources, task, branch_for_check.as_deref());
        let (has_update, commit_hash) = match resolve_head(resolve, &task.name).await {
            Some(v) => v,
            None => {
                // Update on a failure too, otherwise it retries every 5s - and on a
                // timeout it is what lets the next pass reach the triggers behind it.
                update_last_fired(state, &trig, now).await;
                continue;
            }
        };

        let info = get_commit_info(&sources, task, &commit_hash);
        let (msg, _email, author) = tokio::time::timeout(HEAD_RESOLVE_BUDGET, info)
            .await
            .unwrap_or_else(|_| Ok((String::new(), None, String::new())))
            .unwrap_or_else(|_| (String::new(), None, String::new()));

        // Bump tracked flake inputs (OpenPr action) on every due trigger fire,
        // independent of whether HEAD advanced - upstream input updates never
        // move the repo, so gating this on a new commit would never run it.
        // Self-gated: no-ops unless the task qualifies.
        if let Err(e) =
            maybe_trigger_input_update(&state.worker_db, task, commit_hash.clone(), Some(trig.id))
                .await
        {
            warn!(error = %e, trigger_id = %trig.id, "input_update trigger failed");
        }

        // A normal CI evaluation only when there is something new to evaluate;
        // time triggers always re-run against current HEAD.
        if has_update || is_time {
            let trigger_type = cfg.trigger_type();
            match apply_trigger(
                &state.worker_db,
                task,
                ApplyInput {
                    trigger_id: trig.id,
                    trigger_type,
                    commit_hash,
                    commit_message: Some(msg),
                    author_name: Some(author),
                    manual: false,
                    gate_approval: None,
                    repository_override: None,
                    wildcard_override: None,
                    source_comment: None,
                    instance_max_storage_gb: state.config.storage.max_storage_gb,
                },
            )
            .await
            {
                Ok(ApplyOutcome::Created {
                    evaluation: eval,
                    aborted_evaluation,
                    hard_abort,
                }) => {
                    if let Some(aborted_id) = aborted_evaluation {
                        let anchors = if hard_abort {
                            abort_eval_anchors(state, aborted_id).await
                        } else {
                            Vec::new()
                        };

                        scheduler.cancel_evaluation_jobs(aborted_id, &anchors).await;
                    }

                    info!(task = %task.name, trigger_id = %trig.id, evaluation_id = %eval.id, "trigger created evaluation");
                    if let Err(e) =
                        gradient_db::outbox::enqueue_evaluation_created(&state.worker_db, &eval)
                            .await
                    {
                        tracing::error!(error = %e, "failed to enqueue an evaluation's first report");
                    }
                    state.outbox_wake.notify_one();
                }
                Ok(other) => {
                    debug!(task = %task.name, trigger_id = %trig.id, ?other, "trigger applied without creating eval");
                }
                Err(e) => {
                    error!(error = %e, trigger_id = %trig.id, "trigger application failed");
                }
            }
        }

        update_last_fired(state, &trig, now).await;
    }
    Ok(())
}

/// Await one trigger's HEAD resolution under [`HEAD_RESOLVE_BUDGET`]. `None` on
/// either a failure or the timeout: both mean this trigger produced no commit to
/// evaluate, and both must leave the pass free to serve the next one.
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

/// Abort the anchors a hard-aborted evaluation alone still needed; the graph
/// actor owns that write, and the caller cancels the in-memory jobs.
async fn abort_eval_anchors(
    state: &Arc<ServerState>,
    evaluation: EvaluationId,
) -> Vec<DerivationBuildId> {
    match state
        .graph
        .transition(Transition::AbortEvaluationAnchors { evaluation })
        .await
    {
        Ok(report) => report.aborted_anchors,
        Err(e) => {
            warn!(error = %e, evaluation_id = %evaluation, "aborting the evaluation's anchors did not reach the graph actor");
            Vec::new()
        }
    }
}

async fn update_last_fired(state: &Arc<ServerState>, trig: &ept::Model, now: NaiveDateTime) {
    let mut active: ept::ActiveModel = trig.clone().into();
    active.last_fired_at = sea_orm::ActiveValue::Set(Some(now));
    active.updated_at = sea_orm::ActiveValue::Set(now);
    if let Err(e) = active.update(&state.worker_db).await {
        warn!(error = %e, trigger_id = %trig.id, "failed to update last_fired_at");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").unwrap()
    }

    fn tid() -> TaskTriggerId {
        TaskTriggerId::now_v7()
    }

    /// One unreachable remote used to consume the whole 120 s pass budget: the
    /// pass was cancelled before `update_last_fired`, so every trigger behind it
    /// stopped firing - seven days of frozen `last_fired_at` on a live instance.
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
        // 60s interval + max 10% jitter (6s) = at most 66s - 90s is past that.
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
        // Sweep many trigger IDs at the same cycle and confirm none exceed 10%.
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
        // Different last_fired_at on the same trigger should produce different
        // jitter for at least one of two adjacent cycles (probabilistic, but
        // the hash collision rate makes a false negative astronomically rare).
        let id = tid();
        let a = polling_jitter_secs(id, dt("2026-05-06 10:00:00"), 600);
        let b = polling_jitter_secs(id, dt("2026-05-06 10:11:00"), 600);
        let c = polling_jitter_secs(id, dt("2026-05-06 10:22:00"), 600);
        assert!(a != b || a != c, "jitter must vary across cycles");
    }

    #[test]
    fn polling_jitter_zero_when_interval_below_threshold() {
        // 10% of 9 is 0 with integer floor - jitter must be 0.
        assert_eq!(polling_jitter_secs(tid(), dt("2026-05-06 10:00:00"), 9), 0);
    }

    #[test]
    fn polling_does_not_fire_before_interval_plus_jitter() {
        // Pick an interval/timing such that the boundary lies inside the
        // jitter window: interval=100, max jitter=10. At now = last + 100s,
        // the trigger must still wait if its own jitter > 0.
        let id = TaskTriggerId::now_v7();
        let last = dt("2026-05-06 10:00:00");
        let jitter = polling_jitter_secs(id, last, 100);
        if jitter == 0 {
            return; // boundary case validated by other tests
        }
        let just_before = last + chrono::Duration::seconds(100 + jitter as i64 - 1);
        let exactly_at = last + chrono::Duration::seconds(100 + jitter as i64);
        assert!(!polling_due(id, Some(last), 100, just_before));
        assert!(polling_due(id, Some(last), 100, exactly_at));
    }

    #[test]
    fn cron_every_minute_fires_after_minute_boundary() {
        // "0 * * * * *" = every minute at sec=0
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
        // No prior - picks `now - 1 day` as the cursor; daily cron at 02:00
        // should be due if now is past 02:00 today.
        let now = dt("2026-05-06 03:00:00");
        assert!(cron_due("0 0 2 * * *", None, now));
    }
}
