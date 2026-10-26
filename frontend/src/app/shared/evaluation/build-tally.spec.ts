/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { BuildStatusCounts } from '@core/models';
import { buildTally } from './build-tally';

const counts = (c: Partial<BuildStatusCounts>): BuildStatusCounts => ({
  completed: 0, failed: 0, building: 0, queued: 0, substituted: 0, aborted: 0, ...c,
});

describe('buildTally', () => {
  it('counts only the builds the bar shows, with substituted and aborted in the overall total', () => {
    expect(buildTally(counts({ completed: 500, failed: 34, building: 6, queued: 1110, substituted: 5400, aborted: 100 })))
      .toEqual({ done: 534, total: 1650, overall: 7150 });
  });

  it('falls back to the substituted builds when nothing had to be built, matching the full bar', () => {
    expect(buildTally(counts({ substituted: 20 }))).toEqual({ done: 20, total: 20, overall: 20 });
  });
});
