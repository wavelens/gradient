/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::yearly_build_resource_record::YearlyBuildRecordMetric;
use gradient_types::{DerivationBuildId, MDerivationMetric};
use sea_orm::{ConnectionTrait, DbErr, Value};

use super::recap_year_for_build_timestamp;

crate::sql_fn! {
    YEARLY_BUILD_RESOURCE_RECORD_UPSERT_WHEN_BEATS_STORED_SQL = || format!(
        "WITH beats AS MATERIALIZED ( \
           SELECT v.kind, v.value \
           FROM (VALUES ({build_time}::smallint, $5::bigint), ({peak_ram}::smallint, $6::bigint), \
                        ({cpu_time}::smallint, $7::bigint)) v(kind, value) \
           WHERE v.value > coalesce((SELECT r.value FROM yearly_build_resource_record r \
                                     WHERE r.year = $1::integer AND r.kind = v.kind), -1)) \
         INSERT INTO yearly_build_resource_record \
           (id, year, kind, value, derivation, derivation_name, derivation_build, project, \
            project_name, project_display_name, recorded_at) \
         SELECT uuidv7(), $1::integer, b.kind, b.value, $2, $3, $4, p.id, p.name, p.display_name, $8 \
         FROM beats b \
         LEFT JOIN LATERAL ( \
           SELECT pr.id, pr.name, pr.display_name FROM build_job bj \
           JOIN evaluation e ON e.id = bj.evaluation \
           JOIN task t ON t.id = e.task \
           JOIN project pr ON pr.id = t.project \
           WHERE bj.derivation_build = $4 AND EXISTS (SELECT 1 FROM beats) \
           ORDER BY e.created_at LIMIT 1) p ON TRUE \
         ON CONFLICT (year, kind) DO UPDATE SET value = EXCLUDED.value, \
           derivation = EXCLUDED.derivation, derivation_name = EXCLUDED.derivation_name, \
           derivation_build = EXCLUDED.derivation_build, project = EXCLUDED.project, \
           project_name = EXCLUDED.project_name, \
           project_display_name = EXCLUDED.project_display_name, \
           recorded_at = EXCLUDED.recorded_at \
         WHERE EXCLUDED.value > yearly_build_resource_record.value",
        build_time = i16::from(YearlyBuildRecordMetric::BuildTime),
        peak_ram = i16::from(YearlyBuildRecordMetric::PeakRam),
        cpu_time = i16::from(YearlyBuildRecordMetric::CpuTime),
    ),
        params = [Int(2026), DerivationId, Text("hello"), SharedBuildId, Int(1), Int(1), Int(1), Now];
}

pub async fn upsert_yearly_build_resource_record_if_build_beats_longest_time_or_peak_ram_or_cpu<
    C: ConnectionTrait,
>(
    db: &C,
    metric: &MDerivationMetric,
    derivation_build: DerivationBuildId,
) -> Result<(), DbErr> {
    let Some(year) = recap_year_for_build_timestamp(metric.created_at) else {
        return Ok(());
    };
    if metric.build_time_ms.is_none()
        && metric.peak_ram_mb.is_none()
        && metric.cpu_time_ms.is_none()
    {
        return Ok(());
    }
    db.execute_raw(
        YEARLY_BUILD_RESOURCE_RECORD_UPSERT_WHEN_BEATS_STORED_SQL.bind([
            Value::from(i64::from(year)),
            Value::from(metric.derivation.into_inner()),
            Value::from(metric.pname.clone().unwrap_or_default()),
            Value::from(derivation_build.into_inner()),
            Value::from(metric.build_time_ms),
            Value::from(metric.peak_ram_mb),
            Value::from(metric.cpu_time_ms),
            Value::from(metric.created_at),
        ]),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use gradient_entity::yearly_build_resource_record;
    use gradient_fixtures::database::TestDatabase;
    use gradient_types::DerivationId;

    fn build_derivation_metric_row_finished_on_day_three_of_month(
        name: &str,
        build_time_ms: i64,
        peak_ram_mb: Option<i64>,
        month: u32,
    ) -> MDerivationMetric {
        MDerivationMetric {
            derivation: DerivationId::now_v7(),
            pname: Some(name.into()),
            build_time_ms: Some(build_time_ms),
            peak_ram_mb,
            created_at: NaiveDate::from_ymd_opt(2026, month, 3)
                .and_then(|d| d.and_hms_opt(12, 0, 0))
                .expect("a timestamp"),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn yearly_build_resource_record_is_replaced_only_by_build_with_higher_value() {
        let db = TestDatabase::create().await;
        let conn = db.connection();
        let build = DerivationBuildId::now_v7();
        for metric in [
            build_derivation_metric_row_finished_on_day_three_of_month(
                "first",
                5_000,
                Some(900),
                3,
            ),
            build_derivation_metric_row_finished_on_day_three_of_month(
                "shorter",
                4_000,
                Some(1_000),
                4,
            ),
            build_derivation_metric_row_finished_on_day_three_of_month("longest", 9_000, None, 5),
            build_derivation_metric_row_finished_on_day_three_of_month(
                "december",
                99_000,
                Some(99_000),
                12,
            ),
        ] {
            upsert_yearly_build_resource_record_if_build_beats_longest_time_or_peak_ram_or_cpu(
                &conn, &metric, build,
            )
            .await
            .expect("the record upsert runs");
        }

        let mut records: Vec<_> = db
            .rows::<yearly_build_resource_record::Entity>()
            .await
            .into_iter()
            .map(|r| (r.year, r.kind, r.value, r.derivation_name, r.project))
            .collect();
        records.sort_by_key(|r| i16::from(r.1));
        assert_eq!(
            records,
            [
                (
                    2026,
                    YearlyBuildRecordMetric::BuildTime,
                    9_000,
                    "longest".to_owned(),
                    None
                ),
                (
                    2026,
                    YearlyBuildRecordMetric::PeakRam,
                    1_000,
                    "shorter".to_owned(),
                    None
                ),
            ]
        );
        db.finish().await;
    }
}
