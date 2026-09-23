/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { BuildStatus, EvaluationStatus } from '@core/models';
import { buildPhase, evaluationPhase, type StatusPhase } from './status-phase';

describe('evaluationPhase', () => {
  it.each<[EvaluationStatus, StatusPhase]>([
    ['Queued', 'queued'],
    ['Waiting', 'waiting'],
    ['Fetching', 'running'],
    ['EvaluatingFlake', 'running'],
    ['EvaluatingDerivation', 'running'],
    ['Building', 'running'],
    ['Completed', 'success'],
    ['Failed', 'failure'],
    ['Aborted', 'aborted'],
  ])('maps %s to %s', (status, phase) => {
    expect(evaluationPhase(status)).toBe(phase);
  });
});

describe('buildPhase', () => {
  it.each<[BuildStatus, StatusPhase]>([
    ['Created', 'queued'],
    ['Queued', 'queued'],
    ['Building', 'running'],
    ['Completed', 'success'],
    ['Substituted', 'success'],
    ['FailedPermanent', 'failure'],
    ['FailedTransient', 'failure'],
    ['FailedTimeout', 'failure'],
    ['Aborted', 'aborted'],
    ['DependencyFailed', 'aborted'],
    ['Skipped', 'aborted'],
  ])('maps %s to %s', (status, phase) => {
    expect(buildPhase(status)).toBe(phase);
  });
});
