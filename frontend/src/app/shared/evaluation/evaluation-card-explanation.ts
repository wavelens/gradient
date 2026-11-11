/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { EvaluationSummary } from '@core/models';
import { clockTime } from '@shared/text';
import { commitLabel } from './commit';

export interface AbortedEvaluationExplanation {
  text: string;
  replacementId: string | null;
}

type AbortFields = Pick<EvaluationSummary, 'status' | 'abort_reason' | 'aborted_by' | 'superseded_by' | 'finished_at' | 'updated_at'>;
type OriginFields = Pick<EvaluationSummary, 'commit' | 'trigger' | 'retry_of'>;

export function getAbortedEvaluationExplanationWithReplacementId(
  evaluation: AbortFields,
  replacementCommit?: string | null,
): AbortedEvaluationExplanation | null {
  if (evaluation.status !== 'Aborted') return null;
  const { abort_reason: reason, aborted_by: user, superseded_by: replacementId } = evaluation;
  if (reason === 'user' && user) {
    return { text: `Aborted by ${user} at ${clockTime(evaluation.finished_at ?? evaluation.updated_at)}`, replacementId: null };
  }
  if (reason === 'superseded' && replacementId) {
    const commit = replacementCommit ? commitLabel(replacementCommit) : replacementId.slice(0, 8);
    return { text: `New push: ${commit} replaced this evaluation`, replacementId };
  }
  if (reason === 'no_connected_worker_can_build_required_systems') {
    return { text: "Aborted: connected workers can't build the required system", replacementId: null };
  }
  return { text: 'Aborted', replacementId: null };
}

export function evaluationRetryOrRerunLabel(evaluation: OriginFields, olderEvaluations: readonly Pick<EvaluationSummary, 'commit'>[]): string | null {
  if (evaluation.retry_of) return `Retry of ${commitLabel(evaluation.commit)}`;
  const rerun = !evaluation.trigger && !!evaluation.commit && olderEvaluations.some((older) => older.commit === evaluation.commit);
  return rerun ? `Re-run of ${commitLabel(evaluation.commit)}` : null;
}
