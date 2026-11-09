/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { Observable } from 'rxjs';
import type { ConfirmRequest } from '@shared/confirm';
import { commitLabel } from './commit';

export function evaluationAbortTarget(task: string, commit: string | null | undefined): string {
  return `${task} · ${commitLabel(commit)}`;
}

export function buildAbortTargetLine(build: string, task: string): string {
  return `${build} · ${task}`;
}

export function abortEvaluationConfirm<T>(target: string, action: () => Observable<T>): ConfirmRequest<T> {
  return {
    title: 'Abort Evaluation',
    target,
    consequence: 'Running builds that no other evaluation needs get canceled.',
    confirmLabel: 'Abort',
    failure: 'Evaluation abort failed.',
    severity: 'danger',
    action,
  };
}

export function abortBuildConfirm<T>(target: string, action: () => Observable<T>): ConfirmRequest<T> {
  return {
    title: 'Abort Build',
    target,
    consequence: 'This action can stop the build and end the evaluation as aborted.',
    confirmLabel: 'Abort',
    failure: 'Build abort failed.',
    severity: 'danger',
    action,
  };
}
