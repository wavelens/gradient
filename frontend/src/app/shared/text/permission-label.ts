/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

const LABEL_OVERRIDES: Record<string, string> = {
  readStore: 'read cache',
  writeStore: 'write cache',
  manageWebhooks: 'manage event notifications',
  manageCacheWebhooks: 'manage event notifications',
};

const ACRONYMS: ReadonlySet<string> = new Set(['ssh']);

export function permissionLabel(id: string): string {
  return (
    LABEL_OVERRIDES[id] ??
    id
      .replace(/([a-z0-9])([A-Z])/g, '$1 $2')
      .replace(/([A-Z]+)([A-Z][a-z])/g, '$1 $2')
      .replace(/[_-]+/g, ' ')
      .toLowerCase()
      .split(' ')
      .map((word) => (ACRONYMS.has(word) ? word.toUpperCase() : word))
      .join(' ')
  );
}

export function permissionLabelWithCapitalizedFirstLetter(id: string): string {
  const label = permissionLabel(id);
  return label.charAt(0).toUpperCase() + label.slice(1);
}
