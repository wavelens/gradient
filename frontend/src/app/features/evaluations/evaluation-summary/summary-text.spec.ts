/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { EvaluationFailureSummary, FailedBuildSummary } from '@core/services/evaluations.service';
import { blockedLine, failureKind, failureTitle, firstLine, headline } from './summary-text';

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
  it('counts failed, blocked and unevaluated packages as not built', () => {
    expect(headline(summary({ built: 15, failed: 2, blocked: 3 }, 1), false)).toBe('6 packages did not build');
  });

  it('reports a clean evaluation', () => {
    expect(headline(summary({ built: 20 }), false)).toBe('All 20 packages built');
  });

  it('holds the verdict back while the evaluation is running', () => {
    expect(headline(summary({ built: 5, failed: 1, unfinished: 14 }), true))
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
});
