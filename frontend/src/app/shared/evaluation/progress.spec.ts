/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { InputFetch, InputFetchState } from '@core/models';
import { evaluationProgressText, inputFetchLabel, inputFetchRatio } from './progress';

const row = (state: InputFetchState, downloaded_bytes: number, expected_bytes: number): InputFetch =>
  ({ name: 'nixpkgs', state, downloaded_bytes, expected_bytes });

describe('evaluationProgressText', () => {
  it('formats thunks with separators', () => {
    expect(evaluationProgressText({ kind: 'evaluating', thunks: 1234567 })).toBe('Evaluating - 1,234,567 thunks');
  });

  it('has no text for fetching or missing progress', () => {
    expect(evaluationProgressText({ kind: 'fetching', inputs: [] })).toBeNull();
    expect(evaluationProgressText(undefined)).toBeNull();
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
