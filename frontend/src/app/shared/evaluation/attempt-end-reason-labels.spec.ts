/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { buildAttemptEndReasonLabel } from './attempt-end-reason-labels';

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
