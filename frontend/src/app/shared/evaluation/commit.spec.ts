/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { UNKNOWN_COMMIT_LABEL, commitLabel, evaluationTitle } from './commit';

describe('commitLabel', () => {
  it('shortens a real commit hash to eight characters', () => {
    expect(commitLabel('0123456789abcdef')).toBe('01234567');
  });

  it('falls back to the unknown placeholder for a blank or missing commit', () => {
    expect(commitLabel('')).toBe(UNKNOWN_COMMIT_LABEL);
    expect(commitLabel(null)).toBe(UNKNOWN_COMMIT_LABEL);
    expect(commitLabel(undefined)).toBe(UNKNOWN_COMMIT_LABEL);
  });
});

describe('evaluationTitle', () => {
  it('prefers the commit message when present', () => {
    expect(evaluationTitle({ commit: '0123456789abcdef', commit_message: 'Fix build' })).toBe('Fix build');
  });

  it('shows the short hash when there is no message', () => {
    expect(evaluationTitle({ commit: '0123456789abcdef', commit_message: null })).toBe('01234567');
  });

  it('shows the unknown placeholder for an input_update eval with no commit yet', () => {
    expect(evaluationTitle({ commit: '', commit_message: null })).toBe(UNKNOWN_COMMIT_LABEL);
  });
});
