/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

export interface DisplayNameAndName {
  display_name?: string | null;
  name: string;
}

export function getEntityDisplayNameOrName(entity: DisplayNameAndName): string {
  return entity.display_name?.trim() || entity.name;
}

export function formatEntityLabel(entity: DisplayNameAndName, ownerTeam?: DisplayNameAndName | null): string {
  const displayName = entity.display_name?.trim();
  const label = displayName && displayName !== entity.name ? `${displayName} (${entity.name})` : entity.name;
  return ownerTeam ? `${label} · ${getEntityDisplayNameOrName(ownerTeam)}` : label;
}

export function entityToPickerOption(entity: DisplayNameAndName, value: string = entity.name): { label: string; value: string } {
  return { label: formatEntityLabel(entity), value };
}

export function doesSearchQueryMatchEntityNameOrSlug(entity: DisplayNameAndName, query: string): boolean {
  const needle = query.trim().toLowerCase();
  return entity.name.toLowerCase().includes(needle) || (entity.display_name ?? '').toLowerCase().includes(needle);
}
