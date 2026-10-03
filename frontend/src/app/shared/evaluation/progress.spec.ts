/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { InputFetch } from '@core/models';
import { evaluationProgressText, inputFetchLabel, inputFetchRatio } from './progress';

const row = (downloaded_bytes: number, expected_bytes: number): InputFetch =>
  ({ name: 'nixpkgs', state: 'Fetching', downloaded_bytes, expected_bytes });

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
  it('shows a ratio and both sizes when the size is known', () => {
    expect(inputFetchRatio(row(512, 2048))).toBe(0.25);
    expect(inputFetchLabel(row(512, 2048))).toBe('512 B / 2.0 KiB');
  });

  it('shows only the amount when the size is unknown', () => {
    expect(inputFetchRatio(row(512, 0))).toBeNull();
    expect(inputFetchLabel(row(512, 0))).toBe('512 B');
  });
});
