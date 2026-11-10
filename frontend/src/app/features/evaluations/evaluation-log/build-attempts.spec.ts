/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { BuildAttemptLogRow } from '@core/models';
import { buildAttemptOutcomeText, formatBuildAttemptDurationText } from './build-attempts';

const attempt = (fields: Partial<BuildAttemptLogRow>): BuildAttemptLogRow => ({
  id: 'a1',
  outcome: 'Built',
  reason: null,
  worker: null,
  started_at: null,
  finished_at: null,
  created_at: '2026-05-20T12:00:00',
  ...fields,
});

describe('build attempts', () => {
  it('names the failure reason after the outcome', () => {
    expect(buildAttemptOutcomeText(attempt({ outcome: 'Failed', reason: 'Oom' }))).toBe('Failed: out of memory');
    expect(buildAttemptOutcomeText(attempt({ outcome: 'Substituted' }))).toBe('From cache');
  });

  it('shows a duration only for a finished attempt', () => {
    expect(formatBuildAttemptDurationText(attempt({ started_at: '2026-05-20T12:00:00' }))).toBe('');
    expect(formatBuildAttemptDurationText(attempt({ started_at: '2026-05-20T12:00:00', finished_at: '2026-05-20T12:01:30' })))
      .not.toBe('');
  });
});
