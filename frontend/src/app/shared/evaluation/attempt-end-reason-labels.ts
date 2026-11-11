/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { AttemptEndReason } from '@core/models';
import { clockTime } from '@shared/text';

const ATTEMPT_END_REASON_LABEL: Record<AttemptEndReason, string> = {
  completed: 'Completed',
  failed: 'Failed',
  timeout: 'Timed out',
  lost: 'Worker lost',
  aborted: 'Aborted',
};

export function buildAttemptEndReasonLabel(reason: string): string {
  return ATTEMPT_END_REASON_LABEL[reason as AttemptEndReason] ?? reason;
}

export function formatAttemptDropdownLabel(number: number, outcome: string, at?: string | null): string {
  return `Attempt ${number} · ${outcome.toLowerCase()}${at ? ` ${clockTime(at)}` : ''}`;
}
