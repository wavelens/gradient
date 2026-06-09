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
//! DB-driven loop that replaced the legacy `project_poll_loop`.

use chrono::{NaiveDateTime, Utc};

use gradient_core::types::ProjectTriggerId;

/// Upper bound of the jitter added to each polling cycle, expressed as a
/// percentage of `interval_secs`. Spreads concurrently-created triggers out
/// over time so they don't all hit the upstream at the same moment.
const POLLING_JITTER_PCT: u32 = 10;

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
    trigger_id: ProjectTriggerId,
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
    trigger_id: ProjectTriggerId,
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
use std::time::Duration;

use gradient_entity::project_trigger as ept;
use gradient_core::ci::{ApplyInput, ApplyOutcome, apply_trigger};
use gradient_core::sources::{check_project_updates, get_commit_info};
use gradient_core::types::triggers::{TriggerConfig, TriggerType};
use gradient_core::types::*;
use sea_orm::{ActiveModelTrait as _, ColumnTrait, Condition, EntityTrait, QueryFilter};
use tracing::{debug, error, info, warn};

use super::Scheduler;

/// Spawned by `dispatch::start_dispatch_loops`; ticks every 5 s and processes
/// every active polling/time trigger via `dispatch_once`.
pub async fn trigger_dispatch_loop(scheduler: Arc<Scheduler>) {
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    let cancel = scheduler.state.shutdown.token();
    info!("trigger dispatch loop started");
    loop {
        tokio::select! {
            _ = cancel.cancelled() => { info!("trigger dispatch loop shutting down"); return; }
            _ = interval.tick() => {}
        }
        if let Err(e) = dispatch_once(&scheduler).await {
            error!(error = %e, "trigger dispatch error");
        }
    }
}

/// One pass through all active polling+time triggers. Public for tests.
pub(crate) async fn dispatch_once(scheduler: &Scheduler) -> anyhow::Result<()> {
    let state = &scheduler.state;
    let now = gradient_core::types::now();

    let triggers = ept::Entity::find()
        .filter(ept::Column::Active.eq(true))
        .filter(
            Condition::any()
                .add(ept::Column::TriggerType.eq(i16::from(TriggerType::Polling)))
                .add(ept::Column::TriggerType.eq(i16::from(TriggerType::Time))),
        )
        .all(&state.worker_db)
        .await?;
    if triggers.is_empty() {
        return Ok(());
    }

    let project_ids: Vec<_> = triggers
        .iter()
        .map(|t| t.project)
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    let db = &state.worker_db;
    let projects: std::collections::HashMap<_, _> = gradient_core::db::fetch_in_chunks(
        &project_ids,
        |chunk| async move {
            EProject::find()
                .filter(CProject::Id.is_in(chunk))
                .all(db)
                .await
        },
    )
    .await?
    .into_iter()
    .map(|p| (p.id, p))
    .collect();

    for trig in triggers {
        let Some(project) = projects.get(&trig.project) else {
            continue;
        };
        if !project.active {
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

        // Resolve target commit. Polling skips when there's no new commit;
        // time triggers always fire with whatever HEAD currently is.
        let commit_hash =
            match check_project_updates(Arc::clone(state), project, branch_for_check.as_deref())
                .await
            {
                Ok((true, hash)) => hash,
                Ok((false, hash)) if is_time => hash,
                Ok((false, _)) => {
                    update_last_fired(state, &trig, now).await;
                    continue;
                }
                Err(e) => {
                    warn!(error = %e, project = %project.name, "trigger commit resolution failed");
                    // Update on error too, otherwise transient failures retry every 5s.
                    update_last_fired(state, &trig, now).await;
                    continue;
                }
            };

        let (msg, _email, author) = get_commit_info(Arc::clone(state), project, &commit_hash)
            .await
            .unwrap_or_else(|_| (String::new(), None, String::new()));

        let trigger_type = cfg.trigger_type();
        match apply_trigger(
            state.worker_db.inner(),
            project,
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
                aborted_builds,
            }) => {
                if let Some(aborted_id) = aborted_evaluation {
                    scheduler
                        .cancel_evaluation_jobs(aborted_id, &aborted_builds)
                        .await;
                }
                info!(project = %project.name, trigger_id = %trig.id, evaluation_id = %eval.id, "trigger created evaluation");
                gradient_core::ci::actions::dispatch_evaluation_created(state, &eval).await;
            }
            Ok(other) => {
                debug!(project = %project.name, trigger_id = %trig.id, ?other, "trigger applied without creating eval");
            }
            Err(e) => {
                error!(error = %e, trigger_id = %trig.id, "trigger application failed");
            }
        }
        update_last_fired(state, &trig, now).await;
    }
    Ok(())
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

    fn tid() -> ProjectTriggerId {
        ProjectTriggerId::now_v7()
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
        let id = ProjectTriggerId::now_v7();
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
