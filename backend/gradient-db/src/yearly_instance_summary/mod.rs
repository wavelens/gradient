/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod yearly_build_resource_record_upsert;
pub mod yearly_recap_statistics;

use std::time::Duration;

use chrono::{Datelike, NaiveDate, NaiveDateTime, NaiveTime};
use gradient_entity::yearly_instance_recap_snapshot;
use gradient_types::YearlyInstanceRecapSnapshotId;
use gradient_util::supervision::{Pass, PassError};
use sea_orm::sea_query::OnConflict;
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, Set};
use tracing::info;

use crate::DbContext;

pub use yearly_build_resource_record_upsert::upsert_yearly_build_resource_record_if_build_beats_longest_time_or_peak_ram_or_cpu;
pub use yearly_recap_statistics::{
    YearlyRecapStatistics, compute_year_recap_statistics_from_metric_rollups_and_records,
    load_current_public_project_ids,
};

pub fn recap_year_revealed_on_utc_date(today: NaiveDate) -> Option<i32> {
    match (today.month(), today.day()) {
        (12, _) => Some(today.year()),
        (1, 1..=10) => Some(today.year() - 1),
        _ => None,
    }
}

pub fn recap_year_for_build_timestamp(at: NaiveDateTime) -> Option<i32> {
    (at.month() < 12).then_some(at.year())
}

pub fn recap_period_start_and_end_timestamps_for_year(
    year: i32,
) -> Option<(NaiveDateTime, NaiveDateTime)> {
    let start = NaiveDate::from_ymd_opt(year, 1, 1)?.and_time(NaiveTime::MIN);
    let end = NaiveDate::from_ymd_opt(year, 12, 1)?.and_time(NaiveTime::MIN);
    Some((start, end))
}

pub async fn load_recap_snapshot_for_year<C: ConnectionTrait>(
    db: &C,
    year: i32,
) -> Result<Option<yearly_instance_recap_snapshot::Model>, DbErr> {
    yearly_instance_recap_snapshot::Entity::find()
        .filter(yearly_instance_recap_snapshot::Column::Year.eq(year))
        .one(db)
        .await
}

pub async fn create_yearly_instance_recap_snapshot_for_active_year_if_reveal_window_open_and_feature_enabled_and_missing(
    ctx: &DbContext,
    today: NaiveDate,
) -> Result<bool, DbErr> {
    if !ctx.config.server.yearly_recap_enabled {
        return Ok(false);
    }
    let Some(year) = recap_year_revealed_on_utc_date(today) else {
        return Ok(false);
    };
    let db = &ctx.worker_db;
    if load_recap_snapshot_for_year(db, year).await?.is_some() {
        return Ok(false);
    }

    crate::metrics::rollup::run_rollup(ctx).await;
    let (statistics, data_since) =
        compute_year_recap_statistics_from_metric_rollups_and_records(db, year).await?;
    let statistics = serde_json::to_value(&statistics).map_err(|error| {
        DbErr::Custom(format!(
            "Failed to serialize the summary statistics: {error}"
        ))
    })?;
    let snapshot = yearly_instance_recap_snapshot::ActiveModel {
        id: Set(YearlyInstanceRecapSnapshotId::now_v7()),
        year: Set(year),
        created_at: Set(gradient_types::now()),
        data_since: Set(data_since),
        statistics: Set(statistics),
    };
    let inserted = yearly_instance_recap_snapshot::Entity::insert(snapshot)
        .on_conflict(
            OnConflict::column(yearly_instance_recap_snapshot::Column::Year)
                .do_nothing()
                .to_owned(),
        )
        .exec_without_returning(db)
        .await?;
    if inserted > 0 {
        info!(year, "Created yearly recap snapshot");
    }
    Ok(inserted > 0)
}

pub struct YearlyInstanceRecapSnapshotCreationJob {
    ctx: DbContext,
}

impl YearlyInstanceRecapSnapshotCreationJob {
    pub fn new(ctx: DbContext) -> Self {
        Self { ctx }
    }
}

impl Pass for YearlyInstanceRecapSnapshotCreationJob {
    const NAME: &'static str = "yearly_instance_recap_snapshot";
    const BUDGET: Duration = Duration::from_secs(600);
    const RUN_AT_START: bool = true;

    fn period(&self) -> Duration {
        Duration::from_secs(3600)
    }

    async fn run(&self) -> Result<(), PassError> {
        create_yearly_instance_recap_snapshot_for_active_year_if_reveal_window_open_and_feature_enabled_and_missing(
            &self.ctx,
            gradient_types::now().date(),
        )
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Timelike;
    use gradient_fixtures::database::TestDatabase;
    use std::sync::Arc;

    fn naive_date_from_year_month_day(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).expect("a date")
    }

    fn test_db_context_with_yearly_recap_disabled(ctx: &DbContext) -> DbContext {
        let mut config = (*ctx.config).clone();
        config.server.yearly_recap_enabled = false;
        DbContext {
            config: Arc::new(config),
            ..ctx.clone()
        }
    }

    #[test]
    fn reveal_window_is_open_december_1_through_january_10_and_closed_on_november_30_and_january_11()
     {
        assert_eq!(
            recap_year_revealed_on_utc_date(naive_date_from_year_month_day(2026, 11, 30)),
            None
        );
        assert_eq!(
            recap_year_revealed_on_utc_date(naive_date_from_year_month_day(2026, 12, 1)),
            Some(2026)
        );
        assert_eq!(
            recap_year_revealed_on_utc_date(naive_date_from_year_month_day(2026, 12, 31)),
            Some(2026)
        );
        assert_eq!(
            recap_year_revealed_on_utc_date(naive_date_from_year_month_day(2027, 1, 1)),
            Some(2026)
        );
        assert_eq!(
            recap_year_revealed_on_utc_date(naive_date_from_year_month_day(2027, 1, 10)),
            Some(2026)
        );
        assert_eq!(
            recap_year_revealed_on_utc_date(naive_date_from_year_month_day(2027, 1, 11)),
            None
        );
    }

    #[test]
    fn builds_finished_in_december_count_toward_no_recap_year_and_earlier_builds_count_toward_their_calendar_year()
     {
        let midnight_2026_timestamp_from_month_and_day =
            |month, d| naive_date_from_year_month_day(2026, month, d).and_time(NaiveTime::MIN);
        assert_eq!(
            recap_year_for_build_timestamp(midnight_2026_timestamp_from_month_and_day(1, 1)),
            Some(2026)
        );
        assert_eq!(
            recap_year_for_build_timestamp(
                midnight_2026_timestamp_from_month_and_day(11, 30)
                    .with_hour(23)
                    .expect("an hour")
            ),
            Some(2026)
        );
        assert_eq!(
            recap_year_for_build_timestamp(midnight_2026_timestamp_from_month_and_day(12, 1)),
            None
        );
        assert_eq!(
            recap_year_for_build_timestamp(midnight_2026_timestamp_from_month_and_day(12, 31)),
            None
        );
    }

    #[tokio::test]
    async fn create_snapshot_twice_in_reveal_window_keeps_first_snapshot_and_creates_no_second_row()
    {
        let db = TestDatabase::create().await;
        let (ctx, _) = crate::test_ctx::ctx(db.connection()).await;

        let first = create_yearly_instance_recap_snapshot_for_active_year_if_reveal_window_open_and_feature_enabled_and_missing(
            &ctx,
            naive_date_from_year_month_day(2026, 12, 1),
        )
        .await
        .expect("the snapshot pass runs");
        let stored = db.rows::<yearly_instance_recap_snapshot::Entity>().await;
        let again = create_yearly_instance_recap_snapshot_for_active_year_if_reveal_window_open_and_feature_enabled_and_missing(
            &ctx,
            naive_date_from_year_month_day(2027, 1, 10),
        )
        .await
        .expect("the snapshot pass runs again");

        assert!(first);
        assert!(!again);
        assert_eq!(
            db.rows::<yearly_instance_recap_snapshot::Entity>().await,
            stored
        );
        let [snapshot] = stored.as_slice() else {
            panic!("one snapshot: {stored:?}");
        };
        assert_eq!(snapshot.year, 2026);
        assert_eq!(
            snapshot.data_since,
            naive_date_from_year_month_day(2026, 1, 1)
        );
        let statistics: YearlyRecapStatistics =
            serde_json::from_value(snapshot.statistics.clone()).expect("statistics parse");
        assert_eq!(statistics, YearlyRecapStatistics::default());
        crate::test_ctx::settle(ctx).await;
        db.finish().await;
    }

    #[tokio::test]
    async fn snapshot_not_created_outside_reveal_window_or_when_feature_disabled() {
        let db = TestDatabase::create().await;
        let (ctx, _) = crate::test_ctx::ctx(db.connection()).await;
        let disabled = test_db_context_with_yearly_recap_disabled(&ctx);

        for (ctx, today) in [
            (&ctx, naive_date_from_year_month_day(2026, 11, 30)),
            (&ctx, naive_date_from_year_month_day(2027, 1, 11)),
            (&disabled, naive_date_from_year_month_day(2026, 12, 1)),
        ] {
            let created = create_yearly_instance_recap_snapshot_for_active_year_if_reveal_window_open_and_feature_enabled_and_missing(
                ctx, today,
            )
            .await
            .expect("the snapshot pass runs");
            assert!(!created, "{today}");
        }

        assert!(
            db.rows::<yearly_instance_recap_snapshot::Entity>()
                .await
                .is_empty()
        );
        drop(disabled);
        crate::test_ctx::settle(ctx).await;
        db.finish().await;
    }
}
