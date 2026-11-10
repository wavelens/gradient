/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { BuildStatusCounts } from '@core/models';
import { buildTally } from './build-tally';

const counts = (c: Partial<BuildStatusCounts>): BuildStatusCounts => ({
  completed: 0, failed: 0, building: 0, queued: 0, substituted: 0, aborted: 0, dependency_failed: 0, ...c,
});

describe('buildTally', () => {
  it('counts aborted builds as done and substituted builds only in the overall total', () => {
    expect(buildTally(counts({ completed: 500, failed: 34, building: 6, queued: 1110, substituted: 5400, aborted: 100, dependency_failed: 7 }), 'Building'))
      .toEqual({ done: 641, total: 1757, overall: 7157, failed: 34, notBuilt: 7, aborted: 100, counts: expect.any(Object) });
  });

  it('falls back to the substituted builds when nothing had to be built, matching the full bar', () => {
    expect(buildTally(counts({ substituted: 20 }), 'Completed')).toEqual({ done: 20, total: 20, overall: 20, failed: 0, notBuilt: 0, aborted: 0, counts: expect.any(Object) });
  });

  it('counts the unfinished builds of a finished evaluation as aborted, without building stripes', () => {
    const tally = buildTally(counts({ completed: 1, building: 1, queued: 2 }), 'Aborted');
    expect(tally.aborted).toBe(3);
    expect(tally.counts).toEqual(counts({ completed: 1, aborted: 3 }));
  });
});
