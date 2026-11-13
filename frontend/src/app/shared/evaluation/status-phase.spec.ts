/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { BuildStatus, EvaluationStatus, ExecJobStatus } from '@core/models';
import {
  abortedWithoutBuildsTitle, buildOutcomeLabel, buildPhase, entryPointPhase, evaluationPhase, execJobPhase, isEvaluationNotStartedStatus, isPendingBuildStatus,
  PHASE_COLOR, PHASE_SEVERITY, phaseInFinishedEvaluation, type StatusPhase,
} from './status-phase';

describe('phaseInFinishedEvaluation', () => {
  it.each<[StatusPhase, EvaluationStatus, StatusPhase]>([
    ['running', 'Aborted', 'aborted'],
    ['queued', 'Failed', 'aborted'],
    ['waiting', 'Completed', 'aborted'],
    ['failure', 'Aborted', 'failure'],
    ['running', 'Building', 'running'],
  ])('shows %s in a %s evaluation as %s', (phase, status, expected) => {
    expect(phaseInFinishedEvaluation(phase, status)).toBe(expected);
  });
});

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

  it('turns a queued or running evaluation with a waiting reason into waiting, but keeps finished ones', () => {
    expect(evaluationPhase('Building', { kind: 'exec_jobs', waiting_exec_jobs: 1 })).toBe('waiting');
    expect(evaluationPhase('Queued', { kind: 'no_cache' })).toBe('waiting');
    expect(evaluationPhase('Failed', { kind: 'no_cache' })).toBe('failure');
    expect(evaluationPhase('Building', null)).toBe('running');
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

describe('buildOutcomeLabel', () => {
  it('names a build skipped because a dependency failed', () => {
    expect(buildOutcomeLabel('DependencyFailed')).toBe('Skipped: Dependency failure');
    expect(buildOutcomeLabel('FailedPermanent')).toBe('Failed');
  });
});

describe('isEvaluationNotStartedStatus', () => {
  it('counts Queued and Waiting as not started', () => {
    const statuses: EvaluationStatus[] = ['Queued', 'Waiting', 'Fetching', 'Building', 'Aborted'];
    expect(statuses.filter(isEvaluationNotStartedStatus)).toEqual(['Queued', 'Waiting']);
  });
});

describe('isPendingBuildStatus', () => {
  it.each<[BuildStatus, boolean]>([
    ['Created', true],
    ['Queued', true],
    ['Building', true],
    ['Completed', false],
    ['Substituted', false],
    ['FailedPermanent', false],
    ['FailedTransient', false],
    ['FailedTimeout', false],
    ['Aborted', false],
    ['DependencyFailed', false],
    ['Skipped', false],
  ])('treats %s as pending: %s', (status, pending) => {
    expect(isPendingBuildStatus(status)).toBe(pending);
  });
});

describe('entryPointPhase', () => {
  const deps = (building: number) => ({ completed: 0, failed: 0, building, queued: 0, substituted: 0, aborted: 0, dependency_failed: 0 });

  it.each<[BuildStatus, number, StatusPhase]>([
    ['Queued', 1, 'running'],
    ['Created', 2, 'running'],
    ['Queued', 0, 'queued'],
    ['Building', 0, 'running'],
    ['Completed', 1, 'success'],
    ['FailedPermanent', 1, 'failure'],
    ['DependencyFailed', 1, 'aborted'],
  ])('maps %s with %i building deps to %s', (build_status, building, phase) => {
    expect(entryPointPhase({ build_status, deps: deps(building) })).toBe(phase);
  });
});

describe('execJobPhase', () => {
  it.each<[ExecJobStatus, StatusPhase]>([
    ['created', 'waiting'],
    ['queued', 'queued'],
    ['starting', 'running'],
    ['running', 'running'],
    ['completed', 'success'],
    ['failed', 'failure'],
    ['aborted', 'aborted'],
  ])('maps %s to %s', (status, phase) => {
    expect(execJobPhase(status)).toBe(phase);
  });
});

describe('phase colours', () => {
  it.each<[StatusPhase, string, string]>([
    ['queued', 'neutral', '--gr-graph-queued'],
    ['running', 'info', '--gr-graph-running'],
    ['waiting', 'warning', '--gr-status-warning'],
    ['success', 'success', '--gr-graph-success'],
    ['failure', 'danger', '--gr-graph-danger'],
    ['aborted', 'neutral', '--gr-text-secondary'],
  ])('display %s in the color of the %s badge', (phase, severity, role) => {
    expect(PHASE_SEVERITY[phase]).toBe(severity);
    expect(PHASE_COLOR[phase]).toBe(`var(${role})`);
  });
});

describe('abortedWithoutBuildsTitle', () => {
  it('tells an abort before the start apart from an abort before the first build', () => {
    expect(abortedWithoutBuildsTitle(null)).toBe('Aborted before the evaluation started');
    expect(abortedWithoutBuildsTitle('2026-01-01T00:00:00')).toBe('Aborted before any builds started');
  });
});
