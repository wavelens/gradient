/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { TriggerType } from '@core/models';

export function getEvaluationTriggerLabel(evaluation: {
  trigger?: { type: TriggerType } | null;
  triggered_by?: string | null;
  pr_number?: number | null;
}): string {
  const kind = (() => {
    switch (evaluation.trigger?.type) {
      case 'polling': return 'Polling';
      case 'reporter_push': return 'Push';
      case 'reporter_pull_request': return evaluation.pr_number ? `PR #${evaluation.pr_number}` : 'PR';
      case 'time': return 'Schedule';
      default: return 'Manual';
    }
  })();

  return evaluation.triggered_by ? `${kind} · ${evaluation.triggered_by}` : kind;
}
