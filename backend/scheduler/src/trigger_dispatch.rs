/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Per-trigger scheduling: decides whether a trigger is due to fire and
//! drives the unified dispatch loop (replaces the legacy poll loop).
//!
//! This module exposes pure helpers (`polling_due`, `cron_due`) used by the
//! dispatch loop added in a follow-up task. Both take `last_fired_at` and
//! "now" as arguments so the scheduling decision is testable without time
//! manipulation.

use chrono::{NaiveDateTime, Utc};

/// `true` if a polling trigger with the given `interval_secs` should fire
/// at `now`, given that it last fired at `last_fired_at`. A trigger that has
/// never fired (`None`) is always due.
pub(crate) fn polling_due(
    last_fired_at: Option<NaiveDateTime>,
    interval_secs: u32,
    now: NaiveDateTime,
) -> bool {
    match last_fired_at {
        None => true,
        Some(t) => (now - t).num_seconds() >= interval_secs as i64,
    }
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
    let Ok(sched) = Schedule::from_str(cron_expr) else { return false; };
    let after = last_fired_at.unwrap_or(now - chrono::Duration::days(1));
    let after_utc = chrono::DateTime::<Utc>::from_naive_utc_and_offset(after, Utc);
    let now_utc = chrono::DateTime::<Utc>::from_naive_utc_and_offset(now, Utc);
    sched.after(&after_utc).next().map(|next| next <= now_utc).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").unwrap()
    }

    #[test]
    fn polling_no_prior_fires_now() {
        assert!(polling_due(None, 60, dt("2026-05-06 10:00:00")));
    }

    #[test]
    fn polling_under_interval_does_not_fire() {
        assert!(!polling_due(Some(dt("2026-05-06 10:00:00")), 60, dt("2026-05-06 10:00:30")));
    }

    #[test]
    fn polling_at_or_past_interval_fires() {
        assert!(polling_due(Some(dt("2026-05-06 10:00:00")), 60, dt("2026-05-06 10:01:00")));
        assert!(polling_due(Some(dt("2026-05-06 10:00:00")), 60, dt("2026-05-06 10:01:30")));
    }

    #[test]
    fn cron_every_minute_fires_after_minute_boundary() {
        // "0 * * * * *" = every minute at sec=0
        let last = dt("2026-05-06 10:00:30");
        let now  = dt("2026-05-06 10:01:05");
        assert!(cron_due("0 * * * * *", Some(last), now));
    }

    #[test]
    fn cron_does_not_fire_before_next_boundary() {
        let last = dt("2026-05-06 10:01:00");
        let now  = dt("2026-05-06 10:01:30");
        assert!(!cron_due("0 * * * * *", Some(last), now));
    }

    #[test]
    fn cron_invalid_does_not_fire() {
        assert!(!cron_due("garbage", None, dt("2026-05-06 10:00:00")));
    }

    #[test]
    fn cron_no_prior_fires_when_due() {
        // No prior — picks `now - 1 day` as the cursor; daily cron at 02:00
        // should be due if now is past 02:00 today.
        let now = dt("2026-05-06 03:00:00");
        assert!(cron_due("0 0 2 * * *", None, now));
    }
}
