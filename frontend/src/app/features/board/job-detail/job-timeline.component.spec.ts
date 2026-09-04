/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { JobPhase } from '@core/services/board.service';
import { depths, phaseLabel, phaseRows } from './job-timeline.component';

function span(seq: number, parent_seq: number | null, phase: string, start_ms: number, end_ms: number): JobPhase {
  return { seq, parent_seq, phase, start_ms, end_ms, paths: 0, bytes: 0 };
}

describe('job timeline', () => {
  const NESTED: JobPhase[] = [
    { seq: 0, parent_seq: null, phase: 'compress', start_ms: 0, end_ms: 1000, paths: 3, bytes: 0 },
    { seq: 1, parent_seq: 0, phase: 'nar_push', start_ms: 100, end_ms: 900, paths: 3, bytes: 4194304 },
  ];

  it('nests a child phase one level below its parent', () => {
    expect(depths(NESTED)).toEqual([0, 1]);
  });

  it('reports each phase as a share of the job total', () => {
    const rows = phaseRows(NESTED);
    expect(rows[0].durationMs).toBe(1000);
    expect(rows[0].share).toBeCloseTo(1);
    expect(rows[1].durationMs).toBe(800);
    expect(rows[1].share).toBeCloseTo(0.8);
  });

  it('survives a job with no phases', () => {
    expect(phaseRows([])).toEqual([]);
    expect(depths([])).toEqual([]);
  });

  // A truncated timeline can reference a parent that never arrived; the row
  // must still render rather than sending the depth walk into a loop.
  it('treats a dangling parent as a root', () => {
    expect(depths([span(0, 42, 'nar_push', 0, 10)])).toEqual([0]);
  });

  it('does not loop on a span that is its own parent', () => {
    expect(depths([span(0, 0, 'build', 0, 10)])).toEqual([0]);
  });

  it('labels every phase the API can send', () => {
    expect(phaseLabel('nar_push')).toBe('NAR push');
    expect(phaseLabel('unknown_99')).toBe('unknown_99');
  });
});
