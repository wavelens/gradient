/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { ProjectWorkerEntry } from './api.generated';

export type AllowedCapabilities = Pick<
  ProjectWorkerEntry,
  'enable_network' | 'enable_secrets' | 'enable_exec' | 'enable_eval' | 'enable_build'
>;

export const ALLOWED_CAPABILITY_KEYS: (keyof AllowedCapabilities)[] = [
  'enable_network',
  'enable_secrets',
  'enable_exec',
  'enable_eval',
  'enable_build',
];

export const ALL_ALLOWED: AllowedCapabilities = {
  enable_network: true,
  enable_secrets: true,
  enable_exec: false,
  enable_eval: true,
  enable_build: true,
};

export function changedCapabilities(
  before: AllowedCapabilities,
  after: AllowedCapabilities,
): Partial<AllowedCapabilities> {
  return Object.fromEntries(ALLOWED_CAPABILITY_KEYS.filter((k) => before[k] !== after[k]).map((k) => [k, after[k]]));
}

