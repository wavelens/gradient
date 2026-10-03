/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { EvaluationProgress, InputFetch } from '@core/models';
import { formatBytes } from '@shared/text';

export function evaluationProgressText(progress: EvaluationProgress | null | undefined): string | null {
  if (progress?.kind !== 'evaluating') return null;
  return `Evaluating - ${progress.thunks.toLocaleString('en-US')} thunks`;
}

export function inputFetchRatio(input: InputFetch): number | null {
  if (!input.expected_bytes) return null;
  return Math.min(1, input.downloaded_bytes / input.expected_bytes);
}

export function inputFetchLabel(input: InputFetch): string {
  const done = formatBytes(input.downloaded_bytes);
  return input.expected_bytes ? `${done} / ${formatBytes(input.expected_bytes)}` : done;
}
