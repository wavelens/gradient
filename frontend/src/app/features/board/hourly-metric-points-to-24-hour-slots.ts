/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { MetricSeries } from '@shared/ui';
import { clockTime, serverTime } from '@shared/text';
import { windowBuckets } from './storage/storage-chart';

const HOUR_MS = 3_600_000;

export interface HourlyMetricInputSeries<T extends { bucket_start: string }> {
  name: string;
  points: readonly T[];
  pick: (point: T) => number;
  missing: 0 | null;
}

function getLast24HourlySlotStartTimestamps(now: number): number[] {
  return windowBuckets('hour', 24, now - HOUR_MS);
}

function formatHourSlotStartTimestampsAsClockTimeAxisLabels(slots: number[]): string[] {
  return slots.map((slot) => clockTime(slot));
}

function mapMetricPointsToHourSlots<T extends { bucket_start: string }>(
  slots: number[],
  input: HourlyMetricInputSeries<T>,
): (number | null)[] {
  const bySlot = new Map(input.points.map((p) => [serverTime(p.bucket_start).getTime(), input.pick(p)]));
  return slots.map((slot) => bySlot.get(slot) ?? input.missing);
}

export function metricPointsTo24HourChartSeries<T extends { bucket_start: string }>(
  inputs: HourlyMetricInputSeries<T>[],
  now = Date.now(),
): { categories: string[]; series: MetricSeries[] } {
  if (inputs.every((input) => input.points.length === 0)) {
    return { categories: [], series: inputs.map((input) => ({ name: input.name, data: [] })) };
  }
  const slots = getLast24HourlySlotStartTimestamps(now);
  return {
    categories: formatHourSlotStartTimestampsAsClockTimeAxisLabels(slots),
    series: inputs.map((input) => ({ name: input.name, data: mapMetricPointsToHourSlots(slots, input) })),
  };
}
