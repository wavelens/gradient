/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { CacheSubscriptionMode } from '@core/models';

export const SUBSCRIPTION_MODES: { value: CacheSubscriptionMode; label: string }[] = [
  { value: 'ReadWrite', label: 'Read & Write' },
  { value: 'ReadOnly', label: 'Read Only' },
  { value: 'WriteOnly', label: 'Write Only' },
];

export function subscriptionModeLabel(mode: CacheSubscriptionMode): string {
  return SUBSCRIPTION_MODES.find((m) => m.value === mode)?.label ?? mode;
}
