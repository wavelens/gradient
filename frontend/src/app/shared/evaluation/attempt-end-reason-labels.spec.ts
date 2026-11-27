/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { AttemptEndReason } from '@core/models';
import { buildAttemptEndReasonLabel } from './attempt-end-reason-labels';

const EVERY_END_REASON: Record<AttemptEndReason, true> = {
  completed: true,
  failed: true,
  timeout: true,
  lost: true,
  aborted: true,
};

describe('buildAttemptEndReasonLabel', () => {
  it.each(Object.keys(EVERY_END_REASON))('gives %s a readable label instead of the raw value', (reason) => {
    const label = buildAttemptEndReasonLabel(reason);
    expect(label).not.toBe(reason);
    expect(label).toMatch(/^[A-Z]/);
  });

  it('passes an unknown reason through unchanged', () => {
    expect(buildAttemptEndReasonLabel('vanished')).toBe('vanished');
  });
});
