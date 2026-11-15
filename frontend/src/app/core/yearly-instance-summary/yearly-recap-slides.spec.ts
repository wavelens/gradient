/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { YearlyRecapResponse, YearlyRecapStatistics } from '@core/models';
import { type YearlyInstanceSummarySlide, buildNonEmptySlidesFromRecapStatistics, formatBuildTimeForRecap } from './yearly-recap-slides';

function buildYearlyRecapResponse(statistics: YearlyRecapStatistics, dataSince = '2026-01-01'): YearlyRecapResponse {
  return { year: 2026, created_at: '2026-12-01T00:00:00', data_since: dataSince, opened: false, statistics };
}

const EMPTY_TOTALS = { evaluations: 0, builds: 0, build_time_ms: 0, exec_jobs: 0 };

function shownStatistics(slide: YearlyInstanceSummarySlide) {
  return [slide.mainStatistic, ...slide.secondaryStatistics].map(({ value, label, detail }) => ({ value, label, detail }));
}

function slideWithColorScheme(slides: YearlyInstanceSummarySlide[], colorScheme: YearlyInstanceSummarySlide['colorScheme']) {
  const slide = slides.find((s) => s.colorScheme === colorScheme);
  if (!slide) throw new Error(`no ${colorScheme} slide`);
  return slide;
}

describe('buildNonEmptySlidesFromRecapStatistics', () => {
  it('skip empty statistics without showing zero', () => {
    const slides = buildNonEmptySlidesFromRecapStatistics(
      buildYearlyRecapResponse({
        totals: { ...EMPTY_TOTALS, builds: 1, evaluations: 1200, build_time_ms: 5_400 },
        cache: { nars_served: 0, bytes_served: 0, builds_substituted: 0 },
        workers: { machine_time_ms: 0, full_load_time_ms: 0 },
        records: [],
      }),
    );
    expect(slides.map((s) => s.colorScheme)).toEqual(['intro', 'totals', 'summary']);
    expect(shownStatistics(slides[1])).toEqual([
      { value: '1', label: 'build', detail: null },
      { value: '1,200', label: 'evaluations', detail: null },
      { value: '5.4 s', label: 'of build time', detail: null },
    ]);
    expect(slides[1].footnoteText).toBeNull();
    expect(slides[0].mainStatistic.value).toBe('2026');
  });

  it('give the first day with data when coverage starts after January 1', () => {
    const slides = buildNonEmptySlidesFromRecapStatistics(
      buildYearlyRecapResponse({ totals: { ...EMPTY_TOTALS, exec_jobs: 3, exec_jobs_since: '2026-11-02' } }, '2026-03-03'),
    );
    const totals = slideWithColorScheme(slides, 'totals');
    expect(totals.footnoteText).toBe('The totals cover the period from March 3, when data collection began.');
    expect(shownStatistics(totals)[0]).toEqual({ value: '3', label: 'exec jobs', detail: 'Since November 2' });
  });

  it('show a placeholder for hidden private project names', () => {
    const hidden = { public: false };
    const slides = buildNonEmptySlidesFromRecapStatistics(
      buildYearlyRecapResponse({
        most_built_project: { project: hidden, builds: 40 },
        busiest_task: { project: { public: true, name: 'web', display_name: 'Web' }, task: null, builds: 1 },
      }),
    );
    expect(shownStatistics(slideWithColorScheme(slides, 'mostBuilt'))).toEqual([
      { value: 'A private project', label: 'Most built project', detail: '40 builds' },
      { value: 'A private task', label: 'Busiest task', detail: 'Web, 1 build' },
    ]);
  });

  it('sort record builds and format their values', () => {
    const slides = buildNonEmptySlidesFromRecapStatistics(
      buildYearlyRecapResponse({
        records: [
          { kind: 'cpu_time', value: 7_200_000, derivation_name: 'llvm', project: null, recorded_at: '2026-05-01T00:00:00' },
          { kind: 'peak_ram', value: 2048, derivation_name: null, project: { public: true, display_name: 'Kernel' }, recorded_at: '2026-04-01T00:00:00' },
          { kind: 'build_time', value: 90_000, derivation_name: 'chromium', project: { public: true, display_name: 'Browser' }, recorded_at: '2026-03-01T00:00:00' },
        ],
      }),
    );
    expect(shownStatistics(slideWithColorScheme(slides, 'records'))).toEqual([
      { value: '1m 30s', label: 'Longest build', detail: 'chromium in Browser' },
      { value: '2.0 GiB', label: 'Highest peak RAM', detail: 'Kernel' },
      { value: '2h 00m', label: 'Most CPU time', detail: 'llvm' },
    ]);
  });

  it('summarize the main statistic of each slide on the last slide', () => {
    const slides = buildNonEmptySlidesFromRecapStatistics(
      buildYearlyRecapResponse({
        totals: { ...EMPTY_TOTALS, builds: 20, evaluations: 4 },
        cache: { nars_served: 9, bytes_served: 4096, builds_substituted: 0 },
      }),
    );
    const summary = slides.at(-1)!;
    expect(summary.colorScheme).toBe('summary');
    expect(shownStatistics(summary).map((s) => s.value)).toEqual(['20', '4', '4.0 KiB']);
  });

  it('show both worker machine time and full-load worker-minutes on the last slide', () => {
    const slides = buildNonEmptySlidesFromRecapStatistics(
      buildYearlyRecapResponse({
        totals: { ...EMPTY_TOTALS, builds: 20, evaluations: 4 },
        busiest_build_day: { day: '2026-09-17', value: 7_200_000 },
        workers: { machine_time_ms: 3_600_000 * 120, full_load_time_ms: 90 * 60_000, hardest_working: { name: 'builder-1', machine_time_ms: 3_600_000 * 80 } },
      }),
    );
    expect(shownStatistics(slides.at(-1)!)).toEqual([
      { value: '20', label: 'builds', detail: null },
      { value: '4', label: 'evaluations', detail: null },
      { value: 'September 17', label: 'Most build time', detail: '2 hours of builds' },
      { value: '120 hours', label: 'of machine time', detail: null },
      { value: '90', label: 'worker-minutes at full load', detail: null },
    ]);
  });
});

describe('formatBuildTimeForRecap', () => {
  it('show short durations exactly and long durations in whole hours', () => {
    expect(formatBuildTimeForRecap(5_400)).toBe('5.4 s');
    expect(formatBuildTimeForRecap(5_399_999)).toBe('1 hour');
    expect(formatBuildTimeForRecap(3_600_000 * 4321)).toBe('4,321 hours');
  });
});
