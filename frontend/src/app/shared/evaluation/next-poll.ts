/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { TriggerOut } from '@core/models';
import { serverTime } from '@shared/text';

export function getNextPollTime(trigger: Pick<TriggerOut, 'type' | 'active' | 'created_at' | 'last_fired_at' | 'config'>): number | null {
  const intervalSecs = (trigger.config as { interval_secs?: number } | null)?.interval_secs;
  if (!trigger.active || trigger.type !== 'polling' || !intervalSecs) return null;
  const since = serverTime(trigger.last_fired_at ?? trigger.created_at).getTime();
  return Number.isNaN(since) ? null : since + intervalSecs * 1000;
}

export function nextPollText(nextPollAt: string | number | null | undefined, now = Date.now()): string | null {
  if (nextPollAt === null || nextPollAt === undefined) return null;
  const at = typeof nextPollAt === 'number' ? nextPollAt : serverTime(nextPollAt).getTime();
  if (Number.isNaN(at)) return null;
  const minutes = Math.max(1, Math.ceil((at - now) / 60_000));
  const wait = minutes < 60 ? `${minutes}m` : `${Math.floor(minutes / 60)}h ${minutes % 60}m`;
  return `Next poll in ${wait}`;
}
