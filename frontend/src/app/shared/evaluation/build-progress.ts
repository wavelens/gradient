/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { BuildProgress, BuildProgressPhase } from '@core/models';
import { byteAmount } from './progress';

const TITLES: Record<BuildProgressPhase, string> = {
  prefetch: 'Prefetching inputs',
  download: 'Downloading',
  upload: 'Uploading outputs',
};

export function buildProgressTitle(p: BuildProgress): string {
  return TITLES[p.phase];
}

/** Bytes fill the bar when their total is known, paths otherwise; null leaves the bar pulsing. */
export function buildProgressRatio(p: BuildProgress): number | null {
  if (p.bytes_total) return Math.min(1, p.bytes_done / p.bytes_total);
  if (p.paths_total) return Math.min(1, p.paths_done / p.paths_total);
  return null;
}

export function buildProgressBytes(p: BuildProgress): string {
  return p.bytes_done || p.bytes_total ? byteAmount(p.bytes_done, p.bytes_total) : '';
}

export function buildProgressPaths(p: BuildProgress): string | null {
  return p.paths_total && p.paths_total > 1 ? `${p.paths_done} / ${p.paths_total} paths` : null;
}

export function buildPhaseFinished(p: BuildProgress): boolean {
  return p.phase !== 'download' && !!p.paths_total && p.paths_done >= p.paths_total;
}
