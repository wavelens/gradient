/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use gradient_db::exec_jobs::reads::ExecJobPhaseRow;
use gradient_types::MExecJobAttempt;
use sea_orm::ConnectionTrait;

use crate::endpoints::board::{JobPhaseView, job_phases};

/// A reported phase lasts until the next one of the same attempt, the last one until the attempt
/// ends.
fn reported_phase_spans(
    phases: &[&ExecJobPhaseRow],
    origin: NaiveDateTime,
    end: NaiveDateTime,
    first_seq: i32,
) -> Vec<JobPhaseView> {
    let offset = |at: NaiveDateTime| (at - origin).num_milliseconds().max(0);
    let ends = phases
        .iter()
        .skip(1)
        .map(|next| next.created_at)
        .chain([end]);

    phases
        .iter()
        .zip(ends)
        .zip(first_seq..)
        .map(|((phase, until), seq)| JobPhaseView {
            seq,
            parent_seq: None,
            phase: phase.name.clone(),
            start_ms: offset(phase.created_at),
            end_ms: offset(until.max(phase.created_at)),
            paths: 0,
            bytes: 0,
        })
        .collect()
}

pub(crate) async fn worker_timeline<C: ConnectionTrait>(
    db: &C,
    attempt: &MExecJobAttempt,
    phases: &[ExecJobPhaseRow],
) -> Vec<JobPhaseView> {
    with_reported_phases(job_phases(db, attempt.assignment_id).await, attempt, phases)
}

pub(crate) fn with_reported_phases(
    mut timeline: Vec<JobPhaseView>,
    attempt: &MExecJobAttempt,
    phases: &[ExecJobPhaseRow],
) -> Vec<JobPhaseView> {
    let first_seq = timeline.iter().map(|span| span.seq + 1).max().unwrap_or(0);
    let reported: Vec<&ExecJobPhaseRow> = phases
        .iter()
        .filter(|phase| phase.attempt == attempt.id)
        .collect();
    let end = attempt.ended_at.unwrap_or_else(gradient_types::now);
    timeline.extend(reported_phase_spans(
        &reported,
        attempt.started_at,
        end,
        first_seq,
    ));

    timeline
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeDelta;
    use gradient_types::ExecJobAttemptId;

    fn at(origin: NaiveDateTime, secs: i64) -> NaiveDateTime {
        origin + TimeDelta::seconds(secs)
    }

    fn phase(name: &str, created_at: NaiveDateTime) -> ExecJobPhaseRow {
        ExecJobPhaseRow {
            attempt: ExecJobAttemptId::now_v7(),
            name: name.into(),
            created_at,
        }
    }

    #[test]
    fn each_reported_phase_runs_until_the_next_and_the_last_until_the_attempt_ends() {
        let origin = gradient_types::now();
        let rows = [phase("boot", at(origin, 2)), phase("test", at(origin, 5))];
        let refs: Vec<&ExecJobPhaseRow> = rows.iter().collect();

        let spans = reported_phase_spans(&refs, origin, at(origin, 9), 3);

        let shape: Vec<(i32, &str, i64, i64)> = spans
            .iter()
            .map(|s| (s.seq, s.phase.as_str(), s.start_ms, s.end_ms))
            .collect();
        assert_eq!(
            shape,
            [(3, "boot", 2_000, 5_000), (4, "test", 5_000, 9_000)]
        );
    }

    #[test]
    fn reported_phases_of_the_attempt_follow_the_worker_spans() {
        let origin = gradient_types::now();
        let attempt = MExecJobAttempt {
            id: ExecJobAttemptId::now_v7(),
            started_at: origin,
            ended_at: Some(at(origin, 4)),
            ..Default::default()
        };
        let worker_span = JobPhaseView {
            seq: 7,
            parent_seq: None,
            phase: "download".into(),
            start_ms: 0,
            end_ms: 1_000,
            paths: 1,
            bytes: 10,
        };
        let ours = ExecJobPhaseRow {
            attempt: attempt.id,
            ..phase("vm", at(origin, 1))
        };

        let timeline = with_reported_phases(
            vec![worker_span],
            &attempt,
            &[phase("earlier attempt", origin), ours],
        );

        let shape: Vec<(i32, &str)> = timeline.iter().map(|s| (s.seq, s.phase.as_str())).collect();
        assert_eq!(shape, [(7, "download"), (8, "vm")]);
        assert_eq!(timeline[1].end_ms, 4_000);
    }

    #[test]
    fn a_phase_reported_before_the_attempt_start_is_clamped_to_zero() {
        let origin = gradient_types::now();
        let rows = [phase("early", at(origin, -1))];
        let refs: Vec<&ExecJobPhaseRow> = rows.iter().collect();

        let spans = reported_phase_spans(&refs, origin, at(origin, -1), 0);

        assert_eq!((spans[0].start_ms, spans[0].end_ms), (0, 0));
    }
}
