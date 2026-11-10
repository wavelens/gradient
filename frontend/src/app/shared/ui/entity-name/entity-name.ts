/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

export interface DisplayNameAndName {
  display_name?: string | null;
  name: string;
}

export function formatEntityLabel(entity: DisplayNameAndName): string {
  const displayName = entity.display_name?.trim();
  return displayName && displayName !== entity.name ? `${displayName} (${entity.name})` : entity.name;
}

export function entityToPickerOption(entity: DisplayNameAndName, value: string = entity.name): { label: string; value: string } {
  return { label: formatEntityLabel(entity), value };
}

export function doesSearchQueryMatchEntityNameOrSlug(entity: DisplayNameAndName, query: string): boolean {
  const needle = query.trim().toLowerCase();
  return entity.name.toLowerCase().includes(needle) || (entity.display_name ?? '').toLowerCase().includes(needle);
}
