/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use gradient_types::{
    CExecJobAttempt, CExecJobMetric, EExecJobAttempt, EExecJobMetric, ExecJobAttemptId,
    MExecJobMetric,
};
use gradient_wire::types::ExecMetrics;
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbErr, EntityTrait, FromQueryResult, IntoActiveModel,
    QueryFilter, Value,
};

const MIN_BUCKET_WIDTH_MS: i64 = 10_000;
const MAX_CHART_POINTS_PER_ATTEMPT: i64 = 300;

crate::sql! {
    EXEC_ATTEMPT_RESOURCE_USAGE_BUCKETS_WITH_PEAKS_AND_LATEST_TOTAL_RAM_AND_DISK_SIZES_QUERY =
        "WITH windows AS ( \
             SELECT * FROM exec_job_metric WHERE attempt = $1 \
         ), whole_attempt AS ( \
             SELECT COALESCE(sum(cpu_avg::float8 * window_ms) / NULLIF(sum(window_ms), 0), 0)::real \
                      AS whole_attempt_cpu_avg, \
                    max(cpu_max) AS whole_attempt_cpu_max, \
                    max(ram_max_bytes) AS whole_attempt_ram_max_bytes, \
                    max(disk_max_bytes) AS whole_attempt_disk_max_bytes \
             FROM windows \
         ), latest AS ( \
             SELECT ram_total_bytes, disk_total_bytes FROM windows ORDER BY sequence DESC LIMIT 1 \
         ), buckets AS ( \
             SELECT date_bin($2::bigint * interval '1 millisecond', \
                             LEAST(GREATEST(received_at, $3::timestamp), $4::timestamp), \
                             $3::timestamp) AS bucket_start, \
                    COALESCE(sum(cpu_avg::float8 * window_ms) / NULLIF(sum(window_ms), 0), 0)::real \
                      AS cpu_avg, \
                    max(cpu_max) AS cpu_max, \
                    COALESCE(sum(ram_avg_bytes::float8 * window_ms) / NULLIF(sum(window_ms), 0), 0)::bigint \
                      AS ram_avg_bytes, \
                    max(ram_max_bytes) AS ram_max_bytes, \
                    max(disk_max_bytes) AS disk_max_bytes \
             FROM windows GROUP BY 1 \
         ) \
         SELECT (extract(epoch FROM b.bucket_start - $3::timestamp) * 1000)::bigint AS elapsed_ms, \
                b.cpu_avg, b.cpu_max, b.ram_avg_bytes, b.ram_max_bytes, b.disk_max_bytes, \
                w.whole_attempt_cpu_avg, w.whole_attempt_cpu_max, \
                w.whole_attempt_ram_max_bytes, w.whole_attempt_disk_max_bytes, \
                l.ram_total_bytes, l.disk_total_bytes \
         FROM buckets b CROSS JOIN whole_attempt w CROSS JOIN latest l \
         ORDER BY b.bucket_start",
        params = [NewUuid, Int(10_000), Now, Now],
        tier = Bulk;
}

#[derive(Debug, FromQueryResult)]
struct DownsampledResourceUseQueryRow {
    elapsed_ms: i64,
    cpu_avg: f32,
    cpu_max: f32,
    ram_avg_bytes: i64,
    ram_max_bytes: i64,
    disk_max_bytes: i64,
    whole_attempt_cpu_avg: f32,
    whole_attempt_cpu_max: f32,
    whole_attempt_ram_max_bytes: i64,
    whole_attempt_disk_max_bytes: i64,
    ram_total_bytes: i64,
    disk_total_bytes: i64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct WholeAttemptPeakResourceUsage {
    pub cpu_avg: f32,
    pub cpu_max: f32,
    pub ram_max_bytes: u64,
    pub disk_max_bytes: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DownsampledResourceUseBucket {
    pub elapsed_ms: u64,
    pub cpu_avg: f32,
    pub cpu_max: f32,
    pub ram_avg_bytes: u64,
    pub ram_max_bytes: u64,
    pub disk_max_bytes: u64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExecAttemptResourceUsageResult {
    pub peaks: WholeAttemptPeakResourceUsage,
    pub points: Vec<DownsampledResourceUseBucket>,
    pub ram_total_bytes: Option<u64>,
    pub disk_total_bytes: Option<u64>,
}

pub async fn record<C: ConnectionTrait>(
    db: &C,
    attempt: ExecJobAttemptId,
    metrics: &ExecMetrics,
    received_at: NaiveDateTime,
) -> Result<bool, DbErr> {
    let Some(open) = EExecJobAttempt::find_by_id(attempt)
        .filter(CExecJobAttempt::EndedAt.is_null())
        .one(db)
        .await?
    else {
        return Ok(false);
    };
    let row = MExecJobMetric {
        exec_job: open.exec_job,
        attempt,
        sequence: i32::try_from(metrics.sequence).unwrap_or(i32::MAX),
        received_at,
        window_ms: i32::try_from(metrics.window_ms).unwrap_or(i32::MAX),
        cpu_avg: metrics.cpu_avg,
        cpu_max: metrics.cpu_max,
        ram_avg_bytes: u64_byte_count_to_i64_saturating(metrics.ram_avg_bytes),
        ram_max_bytes: u64_byte_count_to_i64_saturating(metrics.ram_max_bytes),
        ram_total_bytes: u64_byte_count_to_i64_saturating(metrics.ram_total_bytes),
        disk_max_bytes: u64_byte_count_to_i64_saturating(metrics.disk_max_bytes),
        disk_total_bytes: u64_byte_count_to_i64_saturating(metrics.disk_total_bytes),
    };
    EExecJobMetric::insert(row.into_active_model())
        .on_conflict(
            OnConflict::columns([CExecJobMetric::Attempt, CExecJobMetric::Sequence])
                .do_nothing()
                .to_owned(),
        )
        .exec_without_returning(db)
        .await?;
    Ok(true)
}

pub async fn get_downsampled_usage_points_and_peaks_for_exec_attempt<C: ConnectionTrait>(
    db: &C,
    attempt: ExecJobAttemptId,
    program_started_at: NaiveDateTime,
    until: NaiveDateTime,
) -> Result<ExecAttemptResourceUsageResult, DbErr> {
    let bucket_width_ms =
        chart_bucket_width_ms_from_run_time((until - program_started_at).num_milliseconds());
    let rows = DownsampledResourceUseQueryRow::find_by_statement(
        EXEC_ATTEMPT_RESOURCE_USAGE_BUCKETS_WITH_PEAKS_AND_LATEST_TOTAL_RAM_AND_DISK_SIZES_QUERY
            .bind([
                Value::Uuid(Some(attempt.into_inner())),
                Value::BigInt(Some(bucket_width_ms)),
                Value::ChronoDateTime(Some(program_started_at)),
                Value::ChronoDateTime(Some(until)),
            ]),
    )
    .all(db)
    .await?;
    let Some(first) = rows.first() else {
        return Ok(ExecAttemptResourceUsageResult::default());
    };
    Ok(ExecAttemptResourceUsageResult {
        peaks: WholeAttemptPeakResourceUsage {
            cpu_avg: first.whole_attempt_cpu_avg,
            cpu_max: first.whole_attempt_cpu_max,
            ram_max_bytes: convert_postgres_bigint_to_u64_zeroing_negatives(
                first.whole_attempt_ram_max_bytes,
            ),
            disk_max_bytes: convert_postgres_bigint_to_u64_zeroing_negatives(
                first.whole_attempt_disk_max_bytes,
            ),
        },
        ram_total_bytes: Some(convert_postgres_bigint_to_u64_zeroing_negatives(
            first.ram_total_bytes,
        )),
        disk_total_bytes: Some(convert_postgres_bigint_to_u64_zeroing_negatives(
            first.disk_total_bytes,
        )),
        points: rows
            .iter()
            .map(|row| DownsampledResourceUseBucket {
                elapsed_ms: convert_postgres_bigint_to_u64_zeroing_negatives(row.elapsed_ms),
                cpu_avg: row.cpu_avg,
                cpu_max: row.cpu_max,
                ram_avg_bytes: convert_postgres_bigint_to_u64_zeroing_negatives(row.ram_avg_bytes),
                ram_max_bytes: convert_postgres_bigint_to_u64_zeroing_negatives(row.ram_max_bytes),
                disk_max_bytes: convert_postgres_bigint_to_u64_zeroing_negatives(
                    row.disk_max_bytes,
                ),
            })
            .collect(),
    })
}

// An extra 1 ms is enough to put a window received exactly at `until` inside the last bucket.
// Runs never split into one bucket more than the limit.
fn chart_bucket_width_ms_from_run_time(run_time_ms: i64) -> i64 {
    (run_time_ms.max(0) / MAX_CHART_POINTS_PER_ATTEMPT + 1).max(MIN_BUCKET_WIDTH_MS)
}

fn u64_byte_count_to_i64_saturating(bytes: u64) -> i64 {
    i64::try_from(bytes).unwrap_or(i64::MAX)
}

fn convert_postgres_bigint_to_u64_zeroing_negatives(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use gradient_entity::ids::DispatchedJobId;
    use gradient_fixtures::database::TestDatabase;
    use gradient_types::{
        EExecJob, EExecJobAttempt, EExecJobMetric, EvaluationId, ExecJobId, MExecJob,
        MExecJobAttempt,
    };
    use sea_orm::EntityTrait;

    async fn create_postgres_database_with_started_open_exec_job_attempt()
    -> (TestDatabase, ExecJobId, MExecJobAttempt) {
        let evaluation = EvaluationId::now_v7();
        let db = gradient_fixtures::shape::shape()
            .evaluation(evaluation)
            .derivation("/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-a.drv")
            .entry_point("apps.x86_64-linux.a")
            .into_database()
            .await;
        let conn = db.connection();
        let point = db
            .entry_point(evaluation, "apps.x86_64-linux.a")
            .await
            .expect("the entry point");
        let now = gradient_types::now();
        let job = crate::rows::insert::<EExecJob, _>(
            &conn,
            MExecJob {
                id: ExecJobId::now_v7(),
                evaluation,
                entry_point: point.id,
                program_derivation: point.derivation,
                requirement: serde_json::json!({}),
                spec: serde_json::json!({}),
                created_at: now,
                ..Default::default()
            },
        )
        .await
        .expect("the exec job is written");
        let attempt = crate::rows::insert::<EExecJobAttempt, _>(
            &conn,
            MExecJobAttempt {
                id: ExecJobAttemptId::now_v7(),
                exec_job: job.id,
                assignment_id: DispatchedJobId::now_v7(),
                worker: "w1".into(),
                number: 1,
                started_at: now,
                program_started_at: Some(now),
                ..Default::default()
            },
        )
        .await
        .expect("the attempt is written");
        (db, job.id, attempt)
    }

    fn exec_metrics_window_with_sequence_and_cpu_max(sequence: u32, cpu_max: f32) -> ExecMetrics {
        ExecMetrics {
            sequence,
            window_ms: 10_000,
            cpu_avg: 0.5,
            cpu_max,
            ram_avg_bytes: 100 << 20,
            ram_max_bytes: 200 << 20,
            ram_total_bytes: 480 << 20,
            disk_max_bytes: 1 << 30,
            disk_total_bytes: 2 << 30,
        }
    }

    #[tokio::test]
    async fn a_resent_window_is_stored_once() {
        let (db, _, attempt) = create_postgres_database_with_started_open_exec_job_attempt().await;
        let conn = db.connection();
        let window = exec_metrics_window_with_sequence_and_cpu_max(0, 1.0);

        let first = record(&conn, attempt.id, &window, gradient_types::now())
            .await
            .expect("the window is recorded");
        let resent = record(&conn, attempt.id, &window, gradient_types::now())
            .await
            .expect("the resent window is accepted");

        assert!(first);
        assert!(resent);
        assert_eq!(db.rows::<EExecJobMetric>().await.len(), 1);
        db.finish().await;
    }

    #[tokio::test]
    async fn windows_of_an_ended_attempt_are_dropped() {
        let (db, _, attempt) = create_postgres_database_with_started_open_exec_job_attempt().await;
        db.execute("UPDATE exec_job_attempt SET ended_at = (now() AT TIME ZONE 'UTC')")
            .await;

        let stored = record(
            &db.connection(),
            attempt.id,
            &exec_metrics_window_with_sequence_and_cpu_max(0, 1.0),
            gradient_types::now(),
        )
        .await
        .expect("the lookup runs");

        assert!(!stored);
        assert!(db.rows::<EExecJobMetric>().await.is_empty());
        db.finish().await;
    }

    #[tokio::test]
    async fn recording_a_window_for_a_missing_attempt_returns_false_and_stores_nothing() {
        let (db, _, _) = create_postgres_database_with_started_open_exec_job_attempt().await;

        let stored = record(
            &db.connection(),
            ExecJobAttemptId::now_v7(),
            &exec_metrics_window_with_sequence_and_cpu_max(0, 1.0),
            gradient_types::now(),
        )
        .await
        .expect("the lookup runs");

        assert!(!stored);
        assert!(db.rows::<EExecJobMetric>().await.is_empty());
        db.finish().await;
    }

    #[tokio::test]
    async fn usage_window_after_chart_end_downsample_to_at_most_300_points_and_keep_the_peak_in_last_point()
     {
        let (db, _, attempt) = create_postgres_database_with_started_open_exec_job_attempt().await;
        let conn = db.connection();
        let program_started_at = attempt.program_started_at.expect("the program started");
        for sequence in 0..1_200_u32 {
            let received_at = program_started_at + Duration::seconds(10 * i64::from(sequence + 1));
            record(
                &conn,
                attempt.id,
                &exec_metrics_window_with_sequence_and_cpu_max(sequence, 1.0),
                received_at,
            )
            .await
            .expect("the window is recorded");
        }
        record(
            &conn,
            attempt.id,
            &exec_metrics_window_with_sequence_and_cpu_max(1_200, 4.0),
            program_started_at + Duration::seconds(12_500),
        )
        .await
        .expect("the late window is recorded");

        let usage = get_downsampled_usage_points_and_peaks_for_exec_attempt(
            &conn,
            attempt.id,
            program_started_at,
            program_started_at + Duration::seconds(12_000),
        )
        .await
        .expect("the usage is read");

        assert!(usage.points.len() <= 300, "{} points", usage.points.len());
        assert_eq!(usage.peaks.cpu_max, 4.0);
        assert_eq!(usage.points.last().map(|point| point.cpu_max), Some(4.0));
        db.finish().await;
    }

    #[tokio::test]
    async fn long_attempts_downsample_to_at_most_300_points_and_keep_the_peak() {
        let (db, _, attempt) = create_postgres_database_with_started_open_exec_job_attempt().await;
        let conn = db.connection();
        let program_started_at = attempt.program_started_at.expect("the program started");
        for sequence in 0..1_200_u32 {
            let cpu_max = if sequence == 777 { 4.0 } else { 1.0 };
            let received_at = program_started_at + Duration::seconds(10 * i64::from(sequence + 1));
            record(
                &conn,
                attempt.id,
                &exec_metrics_window_with_sequence_and_cpu_max(sequence, cpu_max),
                received_at,
            )
            .await
            .expect("the window is recorded");
        }

        let usage = get_downsampled_usage_points_and_peaks_for_exec_attempt(
            &conn,
            attempt.id,
            program_started_at,
            program_started_at + Duration::seconds(12_000),
        )
        .await
        .expect("the usage is read");

        assert!(usage.points.len() <= 300, "{} points", usage.points.len());
        assert!(usage.points.len() >= 299, "{} points", usage.points.len());
        assert_eq!(usage.peaks.cpu_max, 4.0);
        assert_eq!(usage.peaks.cpu_avg, 0.5);
        assert!(usage.points.iter().any(|point| point.cpu_max == 4.0));
        assert!(
            usage
                .points
                .windows(2)
                .all(|pair| pair[0].elapsed_ms < pair[1].elapsed_ms)
        );
        assert_eq!(usage.ram_total_bytes, Some(480 << 20));
        assert_eq!(usage.disk_total_bytes, Some(2 << 30));
        db.finish().await;
    }

    #[tokio::test]
    async fn reading_downsampled_usage_of_an_attempt_without_windows_returns_empty_points_zero_peaks_and_no_total_ram_or_disk_sizes()
     {
        let (db, _, attempt) = create_postgres_database_with_started_open_exec_job_attempt().await;
        let program_started_at = attempt.program_started_at.expect("the program started");

        let usage = get_downsampled_usage_points_and_peaks_for_exec_attempt(
            &db.connection(),
            attempt.id,
            program_started_at,
            program_started_at + Duration::seconds(60),
        )
        .await
        .expect("the usage is read");

        assert_eq!(usage, ExecAttemptResourceUsageResult::default());
        db.finish().await;
    }

    #[tokio::test]
    async fn deleting_the_job_deletes_its_metrics() {
        let (db, job, attempt) =
            create_postgres_database_with_started_open_exec_job_attempt().await;
        let conn = db.connection();
        record(
            &conn,
            attempt.id,
            &exec_metrics_window_with_sequence_and_cpu_max(0, 1.0),
            gradient_types::now(),
        )
        .await
        .expect("the window is recorded");

        EExecJob::delete_by_id(job)
            .exec(&conn)
            .await
            .expect("the job is deleted");

        assert!(db.rows::<EExecJobMetric>().await.is_empty());
        db.finish().await;
    }
}
