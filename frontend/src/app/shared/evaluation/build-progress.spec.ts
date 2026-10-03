/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { BuildProgress } from '@core/models';
import {
  buildPhaseFinished, buildProgressBytes, buildProgressPaths, buildProgressRatio, buildProgressTitle,
} from './build-progress';

const MiB = 1024 * 1024;
const progress = (p: Partial<BuildProgress>): BuildProgress =>
  ({ phase: 'prefetch', bytes_done: 0, bytes_total: null, paths_done: 0, paths_total: null, ...p });

describe('buildProgressTitle', () => {
  it('names each phase', () => {
    expect(buildProgressTitle(progress({ phase: 'prefetch' }))).toBe('Prefetching inputs');
    expect(buildProgressTitle(progress({ phase: 'download' }))).toBe('Downloading');
    expect(buildProgressTitle(progress({ phase: 'upload' }))).toBe('Uploading outputs');
  });
});

describe('buildProgressRatio', () => {
  it('is the transferred share of the announced bytes', () => {
    expect(buildProgressRatio(progress({ bytes_done: 256, bytes_total: 1024, paths_done: 3, paths_total: 4 }))).toBe(0.25);
  });

  it('falls back to the share of paths without a byte total', () => {
    expect(buildProgressRatio(progress({ bytes_done: 256, paths_done: 1, paths_total: 4 }))).toBe(0.25);
  });

  it('is unknown without any total and never passes a whole', () => {
    expect(buildProgressRatio(progress({ bytes_done: 256 }))).toBeNull();
    expect(buildProgressRatio(progress({ bytes_done: 2048, bytes_total: 1024 }))).toBe(1);
  });
});

describe('buildProgress labels', () => {
  it('names both sizes when the total is known, the amount alone otherwise', () => {
    expect(buildProgressBytes(progress({ bytes_done: 120 * MiB, bytes_total: 340 * MiB }))).toBe('120 / 340 MiB');
    expect(buildProgressBytes(progress({ bytes_done: 1536 }))).toBe('1.5 KiB');
    expect(buildProgressBytes(progress({}))).toBe('');
  });

  it('counts paths only when there is more than one', () => {
    expect(buildProgressPaths(progress({ paths_done: 12, paths_total: 40 }))).toBe('12 / 40 paths');
    expect(buildProgressPaths(progress({ paths_done: 0, paths_total: 1 }))).toBeNull();
    expect(buildProgressPaths(progress({}))).toBeNull();
  });

  it('counts the landed paths alone while the total is still growing', () => {
    expect(buildProgressPaths(progress({ paths_done: 12 }))).toBe('12 paths');
    expect(buildProgressPaths(progress({ paths_done: 1 }))).toBeNull();
  });
});

describe('buildPhaseFinished', () => {
  it('ends a phase once every path of a known total landed', () => {
    expect(buildPhaseFinished(progress({ paths_done: 40, paths_total: 40 }))).toBe(true);
    expect(buildPhaseFinished(progress({ phase: 'upload', paths_done: 1, paths_total: 2 }))).toBe(false);
    expect(buildPhaseFinished(progress({ phase: 'download', paths_done: 1, paths_total: 1 }))).toBe(true);
  });

  it('keeps a prefetch whose total is still growing', () => {
    expect(buildPhaseFinished(progress({ paths_done: 12, paths_total: null }))).toBe(false);
  });
});
