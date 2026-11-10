/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { formatCount } from '@shared/text';

/// Red is the failure color across the app. Metrics series avoid red and pink hues. The two closure lines stay away from a red-green pair.
export const metricSeriesChartColorTokenNames = {
  buildTime: '--gr-chart-cyan',
  evalTime: '--gr-chart-purple',
  outputSize: '--gr-chart-green',
  closureSize: '--gr-chart-purple',
  runtimeClosure: '--gr-chart-cyan',
  deps: '--gr-chart-teal',
};

export function formatDependencyCount(count: number): string {
  return `${formatCount(count)} ${count === 1 ? 'dep' : 'deps'}`;
}
