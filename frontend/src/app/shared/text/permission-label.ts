/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

const LABEL_OVERRIDES: Record<string, string> = {
  readStore: 'read cache',
  writeStore: 'write cache',
};

export function permissionLabel(id: string): string {
  return (
    LABEL_OVERRIDES[id] ??
    id
      .replace(/([a-z0-9])([A-Z])/g, '$1 $2')
      .replace(/([A-Z]+)([A-Z][a-z])/g, '$1 $2')
      .replace(/[_-]+/g, ' ')
      .toLowerCase()
  );
}
