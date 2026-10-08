/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { InputFetch, InputFetchState } from '@core/models';
import { inputFetchLabel, inputFetchRatio, phaseProgress, thunkProgress } from './progress';

const row = (state: InputFetchState, downloaded_bytes: number, expected_bytes: number): InputFetch =>
  ({ name: 'nixpkgs', state, downloaded_bytes, expected_bytes });

describe('thunkProgress', () => {
  it('measures the live count against the thunks of the last completed evaluation', () => {
    const row = thunkProgress({ kind: 'evaluating', thunks: 128_032_032 }, 450_000_000);
    expect(row?.label).toBe('128M / 450M thunks');
    expect(row?.percent).toBe(28);
    expect(row?.segments.map(s => s.tone)).toEqual(['building', 'queued']);
  });

  it('pulses at full width without a history or past the last total', () => {
    for (const expected of [null, undefined, 0, 100_000_000]) {
      const row = thunkProgress({ kind: 'evaluating', thunks: 128_032_032 }, expected);
      expect(row?.label).toBe('128M thunks');
      expect(row?.percent).toBeNull();
      expect(row?.segments).toEqual([{ tone: 'building', pct: 100 }]);
    }
  });

  it('has no row for fetching or missing progress', () => {
    expect(thunkProgress({ kind: 'fetching', inputs: [] }, 5)).toBeNull();
    expect(thunkProgress(undefined, 5)).toBeNull();
  });
});

describe('phaseProgress', () => {
  const fetching = { kind: 'fetching' as const, inputs: [] };
  const evaluating = { kind: 'evaluating' as const, thunks: 3 };

  it('takes the first candidate of the kind the status belongs to', () => {
    expect(phaseProgress('Fetching', null, fetching)).toBe(fetching);
    expect(phaseProgress('EvaluatingFlake', fetching, evaluating)).toBe(evaluating);
    expect(phaseProgress('EvaluatingDerivation', fetching)).toBeNull();
  });

  it('has no progress outside the fetching and evaluating statuses', () => {
    expect(phaseProgress('Building', evaluating)).toBeNull();
    expect(phaseProgress('Queued', fetching)).toBeNull();
  });
});

describe('inputFetch', () => {
  it('shows a ratio and both sizes while fetching with a known size', () => {
    expect(inputFetchRatio(row('Fetching', 512, 2048))).toBe(0.25);
    expect(inputFetchLabel(row('Fetching', 512, 2048))).toBe('512 B / 2.0 KiB');
    expect(inputFetchLabel(row('Fetching', 18_400_000, 46_000_000))).toBe('17.5 / 43.9 MiB');
  });

  it('has no ratio and only the amount while fetching with an unknown size', () => {
    expect(inputFetchRatio(row('Fetching', 512, 0))).toBeNull();
    expect(inputFetchLabel(row('Fetching', 512, 0))).toBe('512 B');
    expect(inputFetchLabel(row('Fetching', 0, 0))).toBe('');
  });

  it('shows only the final amount once an input is done', () => {
    expect(inputFetchLabel(row('Done', 2048, 2048))).toBe('2.0 KiB');
    expect(inputFetchLabel(row('Queued', 0, 2048))).toBe('');
  });
});
