/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { RecapBuildRecord, RecapProject, YearlyBuildRecordMetric, YearlyRecapResponse } from '@core/models';
import { formatBytes, formatDuration, formatMebibytes } from '@shared/text';

export type RecapSlideColorSchemeName = 'intro' | 'totals' | 'mostBuilt' | 'records' | 'cache' | 'days' | 'workers' | 'summary';

export interface RecapSlideStatistic {
  label: string;
  value: string;
  countUpTarget: number | null;
  formatCountUpValue: (value: number) => string;
  detail: string | null;
}

export interface YearlyInstanceSummarySlide {
  colorScheme: RecapSlideColorSchemeName;
  headingText: string;
  mainStatistic: RecapSlideStatistic;
  secondaryStatistics: readonly RecapSlideStatistic[];
  footnoteText: string | null;
}

export const yearlyRecapCreditLine = 'NixOS Gradient CI Made OpenSource by Wavelens';

const millisecondsInOneHour = 3_600_000;
const millisecondsInOneMinute = 60_000;

const formatWholeNumber = (value: number) => Math.round(value).toLocaleString('en');

const recapRecordKindLabelTextAndValueFormatterByKind: Record<YearlyBuildRecordMetric, readonly [string, (value: number) => string]> = {
  build_time: ['Longest build', formatDuration],
  peak_ram: ['Highest peak RAM', formatMebibytes],
  cpu_time: ['Most CPU time', formatDuration],
};

const slideRecordKindOrder = Object.keys(recapRecordKindLabelTextAndValueFormatterByKind) as YearlyBuildRecordMetric[];

export function formatBuildTimeForRecap(milliseconds: number): string {
  if (milliseconds < millisecondsInOneHour) return formatDuration(milliseconds);
  const hours = Math.round(milliseconds / millisecondsInOneHour);
  return `${hours.toLocaleString('en')} ${hours === 1 ? 'hour' : 'hours'}`;
}

export function formatIsoDateAsReadableUtcDay(isoDate: string): string {
  return new Date(`${isoDate.slice(0, 10)}T00:00:00Z`).toLocaleDateString('en', { month: 'long', day: 'numeric', timeZone: 'UTC' });
}

export function recapProjectDisplayNameOrPrivateProjectText(project: RecapProject | null | undefined): string {
  return project?.display_name || project?.name || 'A private project';
}

function countedStatistic(count: number, label: string, detail: string | null = null): RecapSlideStatistic {
  return { label, value: formatWholeNumber(count), countUpTarget: count, formatCountUpValue: formatWholeNumber, detail };
}

function measuredStatistic(amount: number, format: (value: number) => string, label: string, detail: string | null = null): RecapSlideStatistic {
  return { label, value: format(amount), countUpTarget: amount, formatCountUpValue: format, detail };
}

function textStatistic(value: string, label: string, detail: string | null = null): RecapSlideStatistic {
  return { label, value, countUpTarget: null, formatCountUpValue: formatWholeNumber, detail };
}

function nounForCount(count: number, singular: string, pluralNoun = `${singular}s`): string {
  return count === 1 ? singular : pluralNoun;
}

function formatWholeNumberWithNoun(count: number, singular: string): string {
  return `${formatWholeNumber(count)} ${nounForCount(count, singular)}`;
}

type RecapBuildRecordWithKind = RecapBuildRecord & { kind: YearlyBuildRecordMetric };

function formatBuildRecordForRecapSlide(record: RecapBuildRecordWithKind): RecapSlideStatistic {
  const [label, format] = recapRecordKindLabelTextAndValueFormatterByKind[record.kind];
  const project = record.project ? recapProjectDisplayNameOrPrivateProjectText(record.project) : null;
  const detail = [record.derivation_name, project].filter(Boolean).join(' in ');
  return measuredStatistic(record.value ?? 0, format, label, detail || null);
}

function slideFromStatistics(
  colorScheme: RecapSlideColorSchemeName,
  headingText: string,
  statistics: readonly RecapSlideStatistic[],
  footnoteText: string | null = null,
): YearlyInstanceSummarySlide | null {
  const [mainStatistic, ...secondaryStatistics] = statistics;
  return mainStatistic ? { colorScheme, headingText, mainStatistic, secondaryStatistics, footnoteText } : null;
}

export function buildNonEmptySlidesFromRecapStatistics(recap: YearlyRecapResponse): YearlyInstanceSummarySlide[] {
  const { year, statistics } = recap;
  const totals = statistics.totals;
  const cache = statistics.cache;
  const workers = statistics.workers;
  const coverageFootnote = (since: string | null | undefined) =>
    since && since > `${year}-01-01` ? `The totals cover the period from ${formatIsoDateAsReadableUtcDay(since)}, when data collection began.` : null;

  const totalStatistics = [
    ...(totals?.builds ? [countedStatistic(totals.builds, nounForCount(totals.builds, 'build'))] : []),
    ...(totals?.evaluations ? [countedStatistic(totals.evaluations, nounForCount(totals.evaluations, 'evaluation'))] : []),
    ...(totals?.build_time_ms ? [measuredStatistic(totals.build_time_ms, formatBuildTimeForRecap, 'of build time')] : []),
    ...(totals?.exec_jobs
      ? [
          countedStatistic(
            totals.exec_jobs,
            nounForCount(totals.exec_jobs, 'exec job'),
            totals.exec_jobs_since && totals.exec_jobs_since > `${year}-01-01` ? `Since ${formatIsoDateAsReadableUtcDay(totals.exec_jobs_since)}` : null,
          ),
        ]
      : []),
  ];

  const mostBuilt = statistics.most_built_project;
  const busiestTask = statistics.busiest_task;
  const mostBuiltStatistics = [
    ...(mostBuilt ? [textStatistic(recapProjectDisplayNameOrPrivateProjectText(mostBuilt.project), 'Most built project', formatWholeNumberWithNoun(mostBuilt.builds ?? 0, 'build'))] : []),
    ...(busiestTask
      ? [textStatistic(busiestTask.task || 'A private task', 'Busiest task', `${recapProjectDisplayNameOrPrivateProjectText(busiestTask.project)}, ${formatWholeNumberWithNoun(busiestTask.builds ?? 0, 'build')}`)]
      : []),
  ];

  const recordStatistics = (statistics.records ?? [])
    .filter((record): record is RecapBuildRecordWithKind => record.kind !== undefined)
    .sort((a, b) => slideRecordKindOrder.indexOf(a.kind) - slideRecordKindOrder.indexOf(b.kind))
    .map(formatBuildRecordForRecapSlide);

  const cacheStatistics = [
    ...(cache?.bytes_served ? [measuredStatistic(cache.bytes_served, formatBytes, 'served from the cache')] : []),
    ...(cache?.nars_served ? [countedStatistic(cache.nars_served, nounForCount(cache.nars_served, 'NAR file') + ' served')] : []),
    ...(cache?.builds_substituted ? [countedStatistic(cache.builds_substituted, nounForCount(cache.builds_substituted, 'build') + ' substituted')] : []),
  ];

  const buildDay = statistics.busiest_build_day;
  const evaluationDay = statistics.busiest_evaluation_day;
  const dayStatistics = [
    ...(buildDay?.day ? [textStatistic(formatIsoDateAsReadableUtcDay(buildDay.day), 'Most build time', `${formatBuildTimeForRecap(buildDay.value ?? 0)} of builds`)] : []),
    ...(evaluationDay?.day ? [textStatistic(formatIsoDateAsReadableUtcDay(evaluationDay.day), 'Most evaluations', formatWholeNumberWithNoun(evaluationDay.value ?? 0, 'evaluation'))] : []),
  ];

  const hardestWorking = workers?.hardest_working;
  const fullLoadMinutes = Math.round((workers?.full_load_time_ms ?? 0) / millisecondsInOneMinute);
  const fullLoadWorkerMinutesStatistic = fullLoadMinutes
    ? countedStatistic(fullLoadMinutes, `worker-${nounForCount(fullLoadMinutes, 'minute')} at full load`)
    : undefined;
  const workerStatistics = [
    ...(workers?.machine_time_ms ? [measuredStatistic(workers.machine_time_ms, formatBuildTimeForRecap, 'of machine time')] : []),
    ...(hardestWorking?.machine_time_ms
      ? [textStatistic(hardestWorking.name || 'One of the workers', 'Hardest-working worker', `${formatBuildTimeForRecap(hardestWorking.machine_time_ms)} of machine time`)]
      : []),
    ...(fullLoadWorkerMinutesStatistic ? [fullLoadWorkerMinutesStatistic] : []),
  ];

  const summaryStatistics = [
    totalStatistics[0],
    totalStatistics[1],
    mostBuiltStatistics[0],
    recordStatistics[0],
    cacheStatistics[0],
    dayStatistics[0],
    workers?.machine_time_ms ? workerStatistics[0] : undefined,
    fullLoadWorkerMinutesStatistic,
  ].filter((statistic): statistic is RecapSlideStatistic => statistic !== undefined);

  const intro: YearlyInstanceSummarySlide = {
    colorScheme: 'intro',
    headingText: 'Look back at everything that this Gradient instance created this year.',
    mainStatistic: textStatistic(String(year), 'Gradient Wrapped'),
    secondaryStatistics: [],
    footnoteText: null,
  };

  return [
    intro,
    slideFromStatistics('totals', 'This instance had a lot of work.', totalStatistics, coverageFootnote(recap.data_since)),
    slideFromStatistics('mostBuilt', 'One project got the most builds.', mostBuiltStatistics),
    slideFromStatistics('records', 'Some builds grew huge.', recordStatistics),
    slideFromStatistics('cache', 'The cache saved a lot of work.', cacheStatistics),
    slideFromStatistics('days', 'Some days were busier than others.', dayStatistics),
    slideFromStatistics('workers', 'Workers worked hard.', workerStatistics, coverageFootnote(workers?.since)),
    slideFromStatistics('summary', `${year} in Review`, summaryStatistics),
  ].filter((slide): slide is YearlyInstanceSummarySlide => slide !== null);
}
