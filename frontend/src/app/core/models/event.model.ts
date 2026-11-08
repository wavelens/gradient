/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { CatalogEntry } from './api.generated';

export interface EventEnvelope<C = Record<string, unknown>> {
  event: string;
  at: string;
  content: C;
}

export interface EventGroup {
  group: string;
  items: { value: string; label: string }[];
}

function familyLabel(family: string): string {
  return family
    .split('_')
    .map((word) => word.charAt(0).toUpperCase() + word.slice(1))
    .join(' ');
}

const WORD_OVERRIDES: Record<string, string> = {
  failed_transient: 'failed, will retry',
  api: 'API',
  cli: 'CLI',
  gc: 'GC',
  nar: 'NAR',
  ssh: 'SSH',
  vm: 'VM',
};

function readableWords(segments: string[]): string {
  const words = segments.flatMap((segment) => WORD_OVERRIDES[segment] ?? segment.split('_').map((word) => WORD_OVERRIDES[word] ?? word));
  const text = words.join(' ');
  return text.charAt(0).toUpperCase() + text.slice(1);
}

/**
 * Readable text for a dot-separated event name. The name build.failed_transient can yield "Build failed, will retry".
 * - Wildcard names stay as they are.
 */
export function eventNameToReadableText(name: string): string {
  return name.includes('*') ? name : readableWords(name.split('.'));
}

/** Durable events grouped by their first dotted segment, optionally limited to `families`. */
export function groupCatalog(entries: CatalogEntry[], families?: string[]): EventGroup[] {
  const groups = new Map<string, EventGroup>();
  for (const { name, durable } of entries) {
    const [family, ...rest] = name.split('.');
    if (!durable || (families && !families.includes(family))) continue;
    const group = familyLabel(family);
    if (!groups.has(group)) groups.set(group, { group, items: [] });
    groups.get(group)!.items.push({ value: name, label: rest.length ? readableWords(rest) : familyLabel(name) });
  }
  return Array.from(groups.values());
}
