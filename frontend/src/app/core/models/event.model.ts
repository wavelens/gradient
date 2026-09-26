/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

export interface EventEnvelope<C = Record<string, unknown>> {
  event: string;
  at: string;
  content: C;
}

export interface EventCatalogEntry {
  name: string;
  durable: boolean;
}

export interface EventGroup {
  group: string;
  items: { value: string; label: string }[];
}

/** Durable events grouped by their first dotted segment, optionally limited to `families`. */
export function groupCatalog(entries: EventCatalogEntry[], families?: string[]): EventGroup[] {
  const groups = new Map<string, EventGroup>();
  for (const { name, durable } of entries) {
    const [family, ...rest] = name.split('.');
    if (!durable || (families && !families.includes(family))) continue;
    const group = family.charAt(0).toUpperCase() + family.slice(1);
    if (!groups.has(group)) groups.set(group, { group, items: [] });
    groups.get(group)!.items.push({ value: name, label: rest.join('.') || name });
  }
  return Array.from(groups.values());
}
