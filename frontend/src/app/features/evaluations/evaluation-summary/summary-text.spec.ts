/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { blockedLine, failureKind, failureTitle, firstLine, getEvaluationSummaryCountTiles, headline } from './summary-text';
import { EvaluationFailureSummary, EvaluationStatus, FailedBuildSummary } from '@core/models';

const summary = (packages: Partial<EvaluationFailureSummary['packages']>, evalErrors = 0): EvaluationFailureSummary => ({
  compared_with: null,
  packages: { total: 20, built: 0, failed: 0, blocked: 0, unfinished: 0, ...packages },
  failures: [],
  failures_total: 0,
  failed_attributes: Array.from({ length: evalErrors }, (_, i) => ({ attr: `a${i}`, message: 'boom', newly_failing: false })),
  fixed: [],
  fixed_total: 0,
});

const failure = (over: Partial<FailedBuildSummary>): FailedBuildSummary => ({
  build_id: 'b1',
  name: 'glibc-2.40',
  derivation_path: '/nix/store/x-glibc-2.40.drv',
  architecture: 'x86_64-linux',
  status: 'FailedPermanent',
  attributes: [],
  blocked: [],
  blocked_total: 0,
  newly_failing: false,
  ...over,
});

describe('evaluation summary text', () => {
  it('counts failed and dependency failed packages, and attributes that failed to evaluate on their own', () => {
    expect(headline(summary({ built: 15, failed: 2, blocked: 3 }, 1), 'Failed')).toBe('5 packages did not build, 1 attribute failed to evaluate');
    expect(headline(summary({ built: 20 }, 2), 'Failed')).toBe('2 attributes failed to evaluate');
  });

  it('names an aborted evaluation and what it left unbuilt', () => {
    expect(headline(summary({ built: 19, unfinished: 1 }), 'Aborted')).toBe('Aborted, 1 package not built');
    expect(headline(summary({ built: 18, failed: 1, unfinished: 1 }), 'Aborted')).toBe('Aborted, 1 package did not build, 1 package not built');
  });

  it('says that an evaluation failed when it stopped before any package', () => {
    expect(headline(summary({ total: 0 }), 'Failed')).toBe('Evaluation failed');
  });

  it('counts nothing for an evaluation that has not started or found no package yet', () => {
    expect(headline(summary({ total: 0 }), 'Waiting')).toBe('Not started yet');
    expect(headline(summary({ total: 0 }), 'Queued')).toBe('Not started yet');
    expect(headline(summary({ total: 0 }), 'EvaluatingFlake')).toBe('Still evaluating');
  });

  it('shows a tile for every non-zero count the headline adds up', () => {
    const tiles = (s: EvaluationFailureSummary, status: EvaluationStatus = 'Completed') =>
      getEvaluationSummaryCountTiles(s, status).map((t) => `${t.label} ${t.value}`);
    expect(tiles(summary({ built: 1, failed: 1, blocked: 1 }, 2))).toEqual(['Packages built 1', 'Failed packages 1', 'Dependency failed 1', 'Failed to evaluate 2']);
    expect(tiles(summary({ built: 5, unfinished: 15 }))).toEqual(['Packages built 5', 'Unbuilt packages 15']);
    expect(tiles(summary({ built: 5, unfinished: 15 }), 'Building')).toEqual(['Packages built 5']);
    expect(tiles(summary({ built: 5 }), 'Waiting')).toEqual([]);
  });

  it('reports a clean evaluation', () => {
    expect(headline(summary({ built: 20 }), 'Completed')).toBe('All 20 packages built');
  });

  it('holds the verdict back while the evaluation is running', () => {
    expect(headline(summary({ built: 5, failed: 1, unfinished: 14 }), 'Building'))
      .toBe('Still running, 5 of 20 packages built so far');
  });

  it('separates a CI fault from a build error', () => {
    expect(failureKind('FailedPermanent')).toEqual({ label: 'Build failed', severity: 'danger' });
    expect(failureKind('FailedTimeout').severity).toBe('warning');
    expect(failureKind('FailedTransient').severity).toBe('warning');
  });

  it('titles a failure by the package attribute, or by the derivation of a dependency', () => {
    expect(failureTitle(failure({ attributes: ['packages.x86_64-linux.curl'] }))).toBe('packages.x86_64-linux.curl');
    expect(failureTitle(failure({}))).toBe('glibc-2.40');
  });

  it('names the first blocked packages and counts the rest', () => {
    const blocked = ['a', 'b', 'c', 'd', 'e', 'f', 'g'];

    expect(blockedLine(failure({ blocked, blocked_total: 30 }))).toBe('Blocks 30 packages: a, b, c, d, e and 25 more');
    expect(blockedLine(failure({ blocked: ['curl'], blocked_total: 1 }))).toBe('Blocks 1 package: curl');
    expect(blockedLine(failure({}))).toBe('');
  });

  it('shows the first line of an evaluation error that has text', () => {
    expect(firstLine('\n  error: attribute missing\n  at flake.nix:3')).toBe('error: attribute missing');
  });

  it('removes terminal colour codes from the first line', () => {
    expect(firstLine('\u001b[31;1merror:\u001b[0m attribute missing')).toBe('error: attribute missing');
  });
});
