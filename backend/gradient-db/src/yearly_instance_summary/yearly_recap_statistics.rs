/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{HashMap, HashSet};

use chrono::{NaiveDate, NaiveDateTime};
use gradient_entity::yearly_build_resource_record::{self, YearlyBuildRecordMetric};
use gradient_types::{CProject, EProject, ETask, ProjectId, TaskId};
use gradient_util::telemetry::metric;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QueryOrder, QueryResult,
    QuerySelect, Value,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use super::recap_period_start_and_end_timestamps_for_year;

crate::sql! {
    PER_METRIC_RECAP_PERIOD_DAY_ROLLUP_COUNT_AND_SUM_QUERY = "SELECT metric, \
        sum(count)::bigint AS c, sum(sum)::double precision AS s \
        FROM metric_rollup \
        WHERE granularity = 2 AND metric = ANY($1::text[]) \
          AND bucket_start >= $2 AND bucket_start < $3 \
        GROUP BY metric",
        params = [Texts("builds.created", 12), Now, Now],
        tier = Sweep;

    METRIC_ROLLUP_FIRST_DAY_QUERY = "SELECT min(bucket_start)::date AS day \
        FROM metric_rollup WHERE granularity = 2 AND metric = ANY($1::text[])",
        params = [Texts("builds.created", 3)],
        tier = Sweep;

    TOP_SCOPE_BY_METRIC_TOTAL_IN_RECAP_PERIOD_SQL = "SELECT \
        (array_agg(scope ORDER BY bucket_start DESC))[1] AS scope, \
        sum(count)::bigint AS c, sum(sum)::double precision AS s \
        FROM metric_rollup \
        WHERE granularity = 2 AND metric = $1 AND bucket_start >= $2 AND bucket_start < $3 \
        GROUP BY scope_hash \
        ORDER BY s DESC, c DESC LIMIT 1",
        params = [Text("builds.created"), Now, Now],
        tier = Sweep;

    RECAP_PERIOD_HIGHEST_METRIC_TOTAL_DAY_QUERY = "SELECT bucket_start::date AS day, \
        sum(count)::bigint AS c, sum(sum)::double precision AS s \
        FROM metric_rollup \
        WHERE granularity = 2 AND metric = ANY($1::text[]) \
          AND bucket_start >= $2 AND bucket_start < $3 \
        GROUP BY bucket_start \
        ORDER BY s DESC, c DESC LIMIT 1",
        params = [Texts("builds.created", 2), Now, Now],
        tier = Sweep;
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct YearlyRecapStatistics {
    pub totals: YearlyEvaluationBuildTimeAndExecJobTotals,
    pub most_built_project: Option<MostBuiltProject>,
    pub busiest_task: Option<BusiestTask>,
    pub records: Vec<RecapBuildRecord>,
    pub cache: YearlyBinaryCacheTotals,
    pub busiest_build_day: Option<BusiestDay>,
    pub busiest_evaluation_day: Option<BusiestDay>,
    pub workers: WorkerTotals,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct YearlyEvaluationBuildTimeAndExecJobTotals {
    pub evaluations: i64,
    pub builds: i64,
    pub build_time_ms: i64,
    pub exec_jobs: i64,
    pub exec_jobs_since: Option<NaiveDate>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct RecapProject {
    pub id: Option<ProjectId>,
    pub name: Option<String>,
    pub display_name: Option<String>,
    pub public: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct MostBuiltProject {
    pub project: RecapProject,
    pub builds: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct BusiestTask {
    pub project: RecapProject,
    pub task: Option<String>,
    pub builds: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct RecapBuildRecord {
    pub kind: YearlyBuildRecordMetric,
    pub value: i64,
    pub derivation_name: Option<String>,
    pub project: Option<RecapProject>,
    pub recorded_at: NaiveDateTime,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct YearlyBinaryCacheTotals {
    pub nars_served: i64,
    pub bytes_served: i64,
    pub builds_substituted: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct BusiestDay {
    pub day: NaiveDate,
    pub value: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct WorkerTotals {
    pub machine_time_ms: i64,
    pub full_load_time_ms: i64,
    pub hardest_working: Option<HardestWorkingWorkerMachineTime>,
    pub since: Option<NaiveDate>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct HardestWorkingWorkerMachineTime {
    pub name: Option<String>,
    pub machine_time_ms: i64,
}

impl RecapProject {
    fn clear_id_and_names_when_not_public(&mut self, public_project_ids: &HashSet<ProjectId>) {
        self.public = self.id.is_some_and(|id| public_project_ids.contains(&id));
        if !self.public {
            *self = Self::default();
        }
    }
}

impl YearlyRecapStatistics {
    pub fn referenced_project_ids(&self) -> HashSet<ProjectId> {
        self.most_built_project
            .iter()
            .map(|most_built| &most_built.project)
            .chain(self.busiest_task.iter().map(|busiest| &busiest.project))
            .chain(self.records.iter().filter_map(|r| r.project.as_ref()))
            .filter_map(|project| project.id)
            .collect()
    }

    pub fn remove_private_names_when_recap_is_limited_to_public_projects(
        &mut self,
        public_project_ids: &HashSet<ProjectId>,
    ) {
        if let Some(most_built) = &mut self.most_built_project {
            most_built
                .project
                .clear_id_and_names_when_not_public(public_project_ids);
        }
        if let Some(busiest) = &mut self.busiest_task {
            busiest
                .project
                .clear_id_and_names_when_not_public(public_project_ids);
            if !busiest.project.public {
                busiest.task = None;
            }
        }
        for record in &mut self.records {
            if let Some(project) = &mut record.project {
                project.clear_id_and_names_when_not_public(public_project_ids);
            }
            if !record.project.as_ref().is_some_and(|p| p.public) {
                record.derivation_name = None;
            }
        }
        if let Some(worker) = &mut self.workers.hardest_working {
            worker.name = None;
        }
    }
}

fn recap_period_start_and_end_as_sql_bind_values(
    start: NaiveDateTime,
    end: NaiveDateTime,
) -> [Value; 2] {
    [Value::from(start), Value::from(end)]
}

fn metric_names_to_sql_text_array_bind_value(metrics: &[&str]) -> Value {
    Value::from(metrics.iter().map(|m| (*m).to_owned()).collect::<Vec<_>>())
}

fn read_summed_count_and_sum_from_row(r: &QueryResult) -> Result<(i64, f64), DbErr> {
    Ok((r.try_get("", "c")?, r.try_get("", "s")?))
}

async fn get_day_metric_rollup_totals_by_metric_name_for_recap_period<C: ConnectionTrait>(
    db: &C,
    metrics: &[&str],
    start: NaiveDateTime,
    end: NaiveDateTime,
) -> Result<HashMap<String, (i64, f64)>, DbErr> {
    let [start, end] = recap_period_start_and_end_as_sql_bind_values(start, end);
    db.query_all_raw(
        PER_METRIC_RECAP_PERIOD_DAY_ROLLUP_COUNT_AND_SUM_QUERY.bind([
            metric_names_to_sql_text_array_bind_value(metrics),
            start,
            end,
        ]),
    )
    .await?
    .iter()
    .map(|r| {
        Ok((
            r.try_get("", "metric")?,
            read_summed_count_and_sum_from_row(r)?,
        ))
    })
    .collect()
}

async fn find_oldest_day_granularity_metric_rollup_date_for_metrics<C: ConnectionTrait>(
    db: &C,
    metrics: &[&str],
) -> Result<Option<NaiveDate>, DbErr> {
    METRIC_ROLLUP_FIRST_DAY_QUERY
        .scalar(
            db,
            [metric_names_to_sql_text_array_bind_value(metrics)],
            "day",
        )
        .await
}

async fn get_scope_with_highest_metric_total_in_recap_period<C: ConnectionTrait>(
    db: &C,
    metric: &str,
    start: NaiveDateTime,
    end: NaiveDateTime,
) -> Result<Option<(serde_json::Value, i64, f64)>, DbErr> {
    let [start, end] = recap_period_start_and_end_as_sql_bind_values(start, end);
    db.query_one_raw(TOP_SCOPE_BY_METRIC_TOTAL_IN_RECAP_PERIOD_SQL.bind([
        Value::from(metric.to_owned()),
        start,
        end,
    ]))
    .await?
    .map(|r| {
        let (count, sum) = read_summed_count_and_sum_from_row(&r)?;
        Ok((r.try_get("", "scope")?, count, sum))
    })
    .transpose()
}

async fn find_day_with_highest_metric_total_and_its_count_and_sum<C: ConnectionTrait>(
    db: &C,
    metrics: &[&str],
    start: NaiveDateTime,
    end: NaiveDateTime,
) -> Result<Option<(NaiveDate, i64, f64)>, DbErr> {
    let [start, end] = recap_period_start_and_end_as_sql_bind_values(start, end);
    db.query_one_raw(RECAP_PERIOD_HIGHEST_METRIC_TOTAL_DAY_QUERY.bind([
        metric_names_to_sql_text_array_bind_value(metrics),
        start,
        end,
    ]))
    .await?
    .map(|r| {
        let (count, sum) = read_summed_count_and_sum_from_row(&r)?;
        Ok((r.try_get("", "day")?, count, sum))
    })
    .transpose()
}

async fn load_current_recap_projects_by_id<C: ConnectionTrait>(
    db: &C,
    ids: HashSet<ProjectId>,
) -> Result<HashMap<ProjectId, RecapProject>, DbErr> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    Ok(EProject::find()
        .filter(CProject::Id.is_in(ids))
        .all(db)
        .await?
        .into_iter()
        .map(|p| {
            let project = RecapProject {
                id: Some(p.id),
                name: Some(p.name),
                display_name: Some(p.display_name),
                public: p.public,
            };
            (p.id, project)
        })
        .collect())
}

pub async fn load_current_public_project_ids<C: ConnectionTrait>(
    db: &C,
    ids: HashSet<ProjectId>,
) -> Result<HashSet<ProjectId>, DbErr> {
    if ids.is_empty() {
        return Ok(HashSet::new());
    }
    Ok(EProject::find()
        .select_only()
        .column(CProject::Id)
        .filter(CProject::Id.is_in(ids))
        .filter(CProject::Public.eq(true))
        .into_tuple::<ProjectId>()
        .all(db)
        .await?
        .into_iter()
        .collect())
}

async fn get_ci_worker_display_name_by_worker_id<C: ConnectionTrait>(
    db: &C,
    worker_id: &str,
) -> Result<String, DbErr> {
    let workers = [worker_id.to_owned()];
    let registered = crate::worker_names::of_registrations(db, &workers, None).await?;
    let named = match registered.into_iter().next() {
        Some(name) => Some(name),
        None => crate::worker_names::of_team_workers(db, &workers, None)
            .await?
            .into_iter()
            .next(),
    };
    Ok(named.map_or_else(|| worker_id.to_owned(), |n| n.display_name))
}

fn read_uuid_from_scope_json_key(scope: &serde_json::Value, key: &str) -> Option<Uuid> {
    scope.get(key)?.as_str()?.parse().ok()
}

fn slide_coverage_start_date(
    oldest: Option<NaiveDate>,
    year_start: NaiveDate,
) -> Option<NaiveDate> {
    oldest.filter(|day| *day > year_start)
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "Float-to-int cast can saturate. Yearly totals stay far below i64::MAX."
)]
fn round_milliseconds_or_bytes_sum_to_i64(sum: f64) -> i64 {
    sum.round() as i64
}

pub async fn compute_year_recap_statistics_from_metric_rollups_and_records<C: ConnectionTrait>(
    db: &C,
    year: i32,
) -> Result<(YearlyRecapStatistics, NaiveDate), DbErr> {
    let (start, end) = recap_period_start_and_end_timestamps_for_year(year)
        .ok_or_else(|| DbErr::Custom(format!("No recap period for {year}")))?;
    let year_start = start.date();

    let totals = get_day_metric_rollup_totals_by_metric_name_for_recap_period(
        db,
        &[
            metric::BUILDS_CREATED,
            metric::BUILDS_DURATION_MS,
            metric::BUILDS_SUBSTITUTED,
            metric::EVALS_COMPLETED,
            metric::EVALS_FAILED,
            metric::EXEC_COMPLETED,
            metric::EXEC_FAILED,
            metric::CACHE_BYTES_SENT,
            metric::WORKER_ASSIGNED_JOB_TIME_MS,
            metric::WORKER_FULL_LOAD_TIME_MS,
        ],
        start,
        end,
    )
    .await?;
    let count = |name: &str| totals.get(name).map_or(0, |(c, _)| *c);
    let sum = |name: &str| {
        totals
            .get(name)
            .map_or(0, |(_, s)| round_milliseconds_or_bytes_sum_to_i64(*s))
    };

    let data_since = find_oldest_day_granularity_metric_rollup_date_for_metrics(
        db,
        &[
            metric::BUILDS_CREATED,
            metric::EVALS_COMPLETED,
            metric::EVALS_FAILED,
        ],
    )
    .await?
    .map_or(year_start, |day| day.max(year_start));
    let exec_jobs_since = slide_coverage_start_date(
        find_oldest_day_granularity_metric_rollup_date_for_metrics(
            db,
            &[metric::EXEC_COMPLETED, metric::EXEC_FAILED],
        )
        .await?,
        year_start,
    );
    let workers_since = slide_coverage_start_date(
        find_oldest_day_granularity_metric_rollup_date_for_metrics(
            db,
            &[metric::WORKER_ASSIGNED_JOB_TIME_MS],
        )
        .await?,
        year_start,
    );

    let top_project =
        get_scope_with_highest_metric_total_in_recap_period(db, metric::BUILDS_CREATED, start, end)
            .await?
            .and_then(|(scope, builds, _)| {
                Some((
                    ProjectId::new(read_uuid_from_scope_json_key(&scope, "project")?),
                    builds,
                ))
            });
    let top_task = get_scope_with_highest_metric_total_in_recap_period(
        db,
        metric::BUILDS_CREATED_PER_TASK,
        start,
        end,
    )
    .await?
    .and_then(|(scope, builds, _)| {
        Some((
            ProjectId::new(read_uuid_from_scope_json_key(&scope, "project")?),
            TaskId::new(read_uuid_from_scope_json_key(&scope, "task")?),
            builds,
        ))
    });
    let records = yearly_build_resource_record::Entity::find()
        .filter(yearly_build_resource_record::Column::Year.eq(year))
        .order_by_asc(yearly_build_resource_record::Column::Kind)
        .all(db)
        .await?;

    let project_ids = top_project
        .iter()
        .map(|(project, _)| *project)
        .chain(top_task.iter().map(|(project, _, _)| *project))
        .chain(records.iter().filter_map(|r| r.project))
        .collect();
    let projects = load_current_recap_projects_by_id(db, project_ids).await?;
    let project_by_id = |id: ProjectId| projects.get(&id).cloned().unwrap_or_default();

    let busiest_task = match top_task {
        Some((project, task, builds)) => Some(BusiestTask {
            project: project_by_id(project),
            task: ETask::find_by_id(task)
                .one(db)
                .await?
                .map(|t| t.display_name),
            builds,
        }),
        None => None,
    };

    let records = records
        .into_iter()
        .map(|r| RecapBuildRecord {
            kind: r.kind,
            value: r.value,
            derivation_name: Some(r.derivation_name).filter(|n| !n.is_empty()),
            project: match r.project.and_then(|id| projects.get(&id)) {
                Some(project) => Some(project.clone()),
                None => r.project_name.as_ref().map(|name| RecapProject {
                    id: None,
                    name: Some(name.clone()),
                    display_name: r.project_display_name.clone(),
                    public: false,
                }),
            },
            recorded_at: r.recorded_at,
        })
        .collect();

    let busiest_build_day = find_day_with_highest_metric_total_and_its_count_and_sum(
        db,
        &[metric::BUILDS_DURATION_MS],
        start,
        end,
    )
    .await?
    .map(|(day, _, ms)| BusiestDay {
        day,
        value: round_milliseconds_or_bytes_sum_to_i64(ms),
    });
    let busiest_evaluation_day = find_day_with_highest_metric_total_and_its_count_and_sum(
        db,
        &[metric::EVALS_COMPLETED, metric::EVALS_FAILED],
        start,
        end,
    )
    .await?
    .map(|(day, evaluations, _)| BusiestDay {
        day,
        value: evaluations,
    });

    let hardest_working = match get_scope_with_highest_metric_total_in_recap_period(
        db,
        metric::WORKER_ASSIGNED_JOB_TIME_MS,
        start,
        end,
    )
    .await?
    {
        Some((scope, _, ms)) => {
            let name = match scope.get("worker").and_then(|w| w.as_str()) {
                Some(worker) => Some(get_ci_worker_display_name_by_worker_id(db, worker).await?),
                None => None,
            };
            Some(HardestWorkingWorkerMachineTime {
                name,
                machine_time_ms: round_milliseconds_or_bytes_sum_to_i64(ms),
            })
        }
        None => None,
    };

    let statistics = YearlyRecapStatistics {
        totals: YearlyEvaluationBuildTimeAndExecJobTotals {
            evaluations: count(metric::EVALS_COMPLETED) + count(metric::EVALS_FAILED),
            builds: count(metric::BUILDS_CREATED),
            build_time_ms: sum(metric::BUILDS_DURATION_MS),
            exec_jobs: count(metric::EXEC_COMPLETED) + count(metric::EXEC_FAILED),
            exec_jobs_since,
        },
        most_built_project: top_project.map(|(project, builds)| MostBuiltProject {
            project: project_by_id(project),
            builds,
        }),
        busiest_task,
        records,
        cache: YearlyBinaryCacheTotals {
            nars_served: count(metric::CACHE_BYTES_SENT),
            bytes_served: sum(metric::CACHE_BYTES_SENT),
            builds_substituted: count(metric::BUILDS_SUBSTITUTED),
        },
        busiest_build_day,
        busiest_evaluation_day,
        workers: WorkerTotals {
            machine_time_ms: sum(metric::WORKER_ASSIGNED_JOB_TIME_MS),
            full_load_time_ms: sum(metric::WORKER_FULL_LOAD_TIME_MS),
            hardest_working,
            since: workers_since,
        },
    };
    Ok((statistics, data_since))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_recap_project_with_public_or_private_visibility(public: bool) -> RecapProject {
        RecapProject {
            id: Some(ProjectId::now_v7()),
            name: Some(if public { "open" } else { "secret" }.into()),
            display_name: Some(if public { "Open" } else { "Secret" }.into()),
            public,
        }
    }

    fn recap_build_record_with_optional_project_and_derivation_name(
        project: Option<RecapProject>,
        name: &str,
    ) -> RecapBuildRecord {
        RecapBuildRecord {
            derivation_name: Some(name.into()),
            project,
            ..Default::default()
        }
    }

    #[test]
    fn public_only_recap_hides_private_project_task_derivation_and_worker_names_but_keeps_public_ones()
     {
        let public = build_recap_project_with_public_or_private_visibility(true);
        let stored_as_private = RecapProject {
            public: false,
            ..public.clone()
        };
        let mut statistics = YearlyRecapStatistics {
            most_built_project: Some(MostBuiltProject {
                project: build_recap_project_with_public_or_private_visibility(true),
                builds: 7,
            }),
            busiest_task: Some(BusiestTask {
                project: stored_as_private.clone(),
                task: Some("Release".into()),
                builds: 5,
            }),
            records: vec![
                recap_build_record_with_optional_project_and_derivation_name(
                    Some(build_recap_project_with_public_or_private_visibility(false)),
                    "secret-app",
                ),
                recap_build_record_with_optional_project_and_derivation_name(
                    Some(stored_as_private),
                    "hello",
                ),
                recap_build_record_with_optional_project_and_derivation_name(None, "orphan"),
            ],
            workers: WorkerTotals {
                hardest_working: Some(HardestWorkingWorkerMachineTime {
                    name: Some("builder-1".into()),
                    machine_time_ms: 1,
                }),
                ..Default::default()
            },
            ..Default::default()
        };

        assert_eq!(statistics.referenced_project_ids().len(), 3);
        let public_project_ids = public.id.into_iter().collect();
        statistics
            .remove_private_names_when_recap_is_limited_to_public_projects(&public_project_ids);

        let most_built = statistics.most_built_project.expect("kept");
        assert_eq!(most_built.project, RecapProject::default());
        assert_eq!(most_built.builds, 7);
        let busiest = statistics.busiest_task.expect("kept");
        assert_eq!(busiest.project, public);
        assert_eq!(busiest.task.as_deref(), Some("Release"));
        let names: Vec<_> = statistics
            .records
            .iter()
            .map(|r| r.derivation_name.as_deref())
            .collect();
        assert_eq!(names, [None, Some("hello"), None]);
        assert_eq!(statistics.records[0].project, Some(RecapProject::default()));
        assert_eq!(statistics.records[1].project, Some(public));
        assert_eq!(
            statistics.workers.hardest_working.and_then(|w| w.name),
            None
        );
    }

    #[test]
    fn test_reports_coverage_start_date_only_when_oldest_data_starts_after_recap_year_start() {
        let january = NaiveDate::from_ymd_opt(2026, 1, 1).expect("a date");
        let march = NaiveDate::from_ymd_opt(2026, 3, 4).expect("a date");
        let earlier = NaiveDate::from_ymd_opt(2025, 6, 1).expect("a date");
        assert_eq!(slide_coverage_start_date(Some(march), january), Some(march));
        assert_eq!(slide_coverage_start_date(Some(earlier), january), None);
        assert_eq!(slide_coverage_start_date(None, january), None);
    }
}
