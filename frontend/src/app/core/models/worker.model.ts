/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { ProjectWorkerEntry } from './api.generated';

export type AllowedCapabilities = Pick<
  ProjectWorkerEntry,
  'enable_fetch' | 'enable_eval' | 'enable_build'
>;

export function changedCapabilities(
  before: AllowedCapabilities,
  after: AllowedCapabilities,
): Partial<AllowedCapabilities> {
  const keys: (keyof AllowedCapabilities)[] = ['enable_fetch', 'enable_eval', 'enable_build'];
  return Object.fromEntries(keys.filter((k) => before[k] !== after[k]).map((k) => [k, after[k]]));
}

