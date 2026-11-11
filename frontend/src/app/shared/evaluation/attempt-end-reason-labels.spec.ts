/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { clockTime } from '@shared/text';
import { buildAttemptEndReasonLabel, formatAttemptDropdownLabel } from './attempt-end-reason-labels';

describe('buildAttemptEndReasonLabel', () => {
  it.each([
    ['timeout', 'Timed out'],
    ['lost', 'Worker lost'],
    ['failed', 'Failed'],
    ['vanished', 'vanished'],
  ])('labels %s as %s', (reason, label) => {
    expect(buildAttemptEndReasonLabel(reason)).toBe(label);
  });
});

describe('formatAttemptDropdownLabel', () => {
  it('names the attempt, its outcome and the time it ended', () => {
    const at = '2026-11-11T15:13:00';
    expect(formatAttemptDropdownLabel(1, 'Failed', at)).toBe(`Attempt 1 · failed ${clockTime(at)}`);
    expect(formatAttemptDropdownLabel(2, 'Running')).toBe('Attempt 2 · running');
  });
});
