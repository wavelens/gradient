/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { BuildStatusCounts } from '@core/models';
import { buildTally, getBuildCountTextPieces } from './build-tally';

const counts = (c: Partial<BuildStatusCounts>): BuildStatusCounts => ({
  completed: 0, failed: 0, building: 0, queued: 0, substituted: 0, aborted: 0, dependency_failed: 0, ...c,
});

describe('buildTally', () => {
  it('counts every build to build against one total and leaves substituted builds to the overall count', () => {
    expect(buildTally(counts({ completed: 500, failed: 34, building: 6, queued: 1110, substituted: 5400, aborted: 100, dependency_failed: 7 }), 'Building'))
      .toEqual(expect.objectContaining({ built: 500, total: 1757, overall: 7157, failed: 34, skipped: 7, aborted: 100 }));
  });

  it('falls back to the substituted builds when nothing had to be built, matching the full bar', () => {
    expect(buildTally(counts({ substituted: 20 }), 'Completed')).toEqual(expect.objectContaining({ built: 20, total: 20, overall: 20, failed: 0, skipped: 0, aborted: 0 }));
  });

  it('counts the unfinished builds of a finished evaluation as aborted, without building stripes', () => {
    const tally = buildTally(counts({ completed: 1, building: 1, queued: 2 }), 'Aborted');
    expect(tally.aborted).toBe(3);
    expect(tally.counts).toEqual(counts({ completed: 1, aborted: 3 }));
  });
});

describe('getBuildCountTextPieces', () => {
  it('never counts a failed or skipped build as built and leaves out empty parts', () => {
    const pieces = getBuildCountTextPieces(buildTally(counts({ completed: 1, failed: 1, dependency_failed: 1 }), 'Failed'));
    expect(pieces.map((piece) => piece.text)).toEqual(['1 of 3 built', '1 failed', '1 skipped']);
  });

  it('names the unit when one is given', () => {
    expect(getBuildCountTextPieces({ built: 1, total: 1, failed: 0, skipped: 0, aborted: 0 }, 'package')[0].text).toBe('1 of 1 package built');
  });
});
