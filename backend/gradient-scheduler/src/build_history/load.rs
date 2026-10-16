/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use gradient_pool::estimate::Sample;
use gradient_pool::score::contention_factor;
use gradient_pool::score::weights::REFERENCE_CORE_SCORE;
use gradient_types::ids::DerivationId;
use sea_orm::{ConnectionTrait, DbErr, FromQueryResult};

use super::store::LoadedSample;

const REFRESH_OVERLAP: chrono::Duration = chrono::Duration::minutes(5);

gradient_db::sql! {
    BUILD_SAMPLES_SINCE = r#"
SELECT m.id, m.created_at, m.derivation, m.architecture, coalesce(m.pname, '') AS pname,
       coalesce(m.input_pnames, '{}') AS input_pnames, m.build_cores, m.peak_ram_mb,
       m.build_time_ms, m.concurrent_builds, m.cpu_core_score, m.oom_killed,
       (SELECT sum(o.nar_size) FROM derivation_output o WHERE o.derivation = m.derivation)::bigint
           AS output_nar_bytes
FROM derivation_metric m
WHERE m.created_at >= $1
ORDER BY m.created_at
"#,
        params = [Now];
}

#[derive(FromQueryResult)]
struct SampleRow {
    id: uuid::Uuid,
    created_at: NaiveDateTime,
    derivation: uuid::Uuid,
    architecture: String,
    pname: String,
    input_pnames: Vec<String>,
    build_cores: Option<i32>,
    peak_ram_mb: Option<i64>,
    build_time_ms: Option<i64>,
    concurrent_builds: Option<i32>,
    cpu_core_score: Option<i32>,
    oom_killed: bool,
    output_nar_bytes: Option<i64>,
}

pub(crate) async fn load_since(
    db: &impl ConnectionTrait,
    newest: Option<NaiveDateTime>,
) -> Result<Vec<LoadedSample>, DbErr> {
    let rows = SampleRow::find_by_statement(BUILD_SAMPLES_SINCE.bind([read_since(newest).into()]))
        .all(db)
        .await?;

    Ok(rows.into_iter().map(SampleRow::into_loaded).collect())
}

fn read_since(newest: Option<NaiveDateTime>) -> NaiveDateTime {
    newest.map_or(chrono::DateTime::UNIX_EPOCH.naive_utc(), |t| {
        t - REFRESH_OVERLAP
    })
}

impl SampleRow {
    fn into_loaded(self) -> LoadedSample {
        let positive = |v: Option<i64>| v.and_then(|v| u64::try_from(v).ok()).filter(|v| *v > 0);
        LoadedSample {
            id: self.id,
            created_at: self.created_at,
            oom_killed: self.oom_killed,
            sample: Sample {
                derivation: DerivationId::new(self.derivation),
                architecture: self.architecture,
                pname: self.pname,
                input_pnames: self.input_pnames,
                cores: self.build_cores.and_then(|c| u32::try_from(c).ok()),
                peak_ram_mb: positive(self.peak_ram_mb),
                build_ms: self
                    .build_time_ms
                    .map(|ms| normalized_build_ms(ms, self.concurrent_builds, self.cpu_core_score)),
                output_nar_bytes: positive(self.output_nar_bytes),
                lower_bound: self.oom_killed || self.build_time_ms.is_none(),
            },
        }
    }
}

fn normalized_build_ms(
    build_time_ms: i64,
    concurrent: Option<i32>,
    core_score: Option<i32>,
) -> f64 {
    let others = concurrent.and_then(|c| u32::try_from(c).ok()).unwrap_or(0);
    let uncontended = build_time_ms as f64 / contention_factor(others);
    let built_on = core_score
        .filter(|s| *s > 0)
        .map_or(REFERENCE_CORE_SCORE, f64::from);
    uncontended * built_on / REFERENCE_CORE_SCORE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_read_starts_inside_the_range_of_a_postgres_timestamp() {
        assert_eq!(read_since(None), chrono::DateTime::UNIX_EPOCH.naive_utc());
        let newest = gradient_types::now();
        assert_eq!(read_since(Some(newest)), newest - REFRESH_OVERLAP);
    }

    #[test]
    fn a_build_time_is_moved_to_the_reference_core_score_without_the_builds_beside_it() {
        let alone_on_reference = normalized_build_ms(100_000, Some(0), Some(4_000));
        let on_a_core_twice_as_fast = normalized_build_ms(100_000, None, Some(8_000));
        let beside_five = normalized_build_ms(100_000, Some(5), None);

        assert_eq!(alone_on_reference, 100_000.0);
        assert_eq!(on_a_core_twice_as_fast, 200_000.0);
        assert!(beside_five < 100_000.0);
    }
}
