/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { TriggerType } from '@core/models';

export function getCiTriggerTypeLabel(type: TriggerType): string {
  switch (type) {
    case 'polling': return 'Polling';
    case 'reporter_push': return 'Push';
    case 'reporter_pull_request': return 'Pull request';
    case 'time': return 'Schedule (cron)';
  }
}

export function getEvaluationTriggerLabel(evaluation: {
  trigger?: { type: TriggerType } | null;
  triggered_by?: string | null;
  pr_number?: number | null;
}): string {
  const type = evaluation.trigger?.type;
  const kind = !type
    ? 'Manual'
    : type === 'reporter_pull_request'
      ? ['PR', evaluation.pr_number && `#${evaluation.pr_number}`].filter(Boolean).join(' ')
      : getCiTriggerTypeLabel(type);

  return evaluation.triggered_by ? `${kind} · ${evaluation.triggered_by}` : kind;
}
