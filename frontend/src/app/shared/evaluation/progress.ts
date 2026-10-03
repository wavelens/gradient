/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { EvaluationProgress, EvaluationStatus, InputFetch } from '@core/models';
import { formatBytes } from '@shared/text';

const PHASE_KIND: Partial<Record<EvaluationStatus, EvaluationProgress['kind']>> = {
  Fetching: 'fetching',
  EvaluatingFlake: 'evaluating',
  EvaluatingDerivation: 'evaluating',
};

export function phaseProgress(
  status: EvaluationStatus,
  ...candidates: (EvaluationProgress | null | undefined)[]
): EvaluationProgress | null {
  const kind = PHASE_KIND[status];
  if (!kind) return null;
  return candidates.find(p => p?.kind === kind) ?? null;
}

export function evaluationProgressText(progress: EvaluationProgress | null | undefined): string | null {
  if (progress?.kind !== 'evaluating') return null;
  return `Evaluating - ${progress.thunks.toLocaleString('en-US')} thunks`;
}

export function inputFetchRatio(input: InputFetch): number | null {
  if (!input.expected_bytes) return null;
  return Math.min(1, input.downloaded_bytes / input.expected_bytes);
}

export function inputFetchLabel(input: InputFetch): string {
  if (!input.downloaded_bytes) return '';
  const done = formatBytes(input.downloaded_bytes);
  if (input.state !== 'Fetching' || !input.expected_bytes) return done;
  const total = formatBytes(input.expected_bytes);
  const unit = done.slice(done.indexOf(' '));
  return total.endsWith(unit) ? `${done.slice(0, -unit.length)} / ${total}` : `${done} / ${total}`;
}
