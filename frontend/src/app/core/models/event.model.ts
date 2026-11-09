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
    .map((word) => WORD_OVERRIDES[word] ?? word.charAt(0).toUpperCase() + word.slice(1))
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

export function eventNameMatchesAnyPrefix(name: string, eventNamePrefixes: readonly string[]): boolean {
  return eventNamePrefixes.some((prefix) => name === prefix || name.startsWith(`${prefix}.`));
}

/** Group durable events by their first dot-separated segment. If `eventNamePrefixes` is present, keep only names equal to the prefix or names that start with the prefix followed by a dot. */
export function groupCatalog(entries: CatalogEntry[], eventNamePrefixes?: readonly string[]): EventGroup[] {
  const groups = new Map<string, EventGroup>();
  for (const { name, durable } of entries) {
    const [family, ...rest] = name.split('.');
    const offered = !eventNamePrefixes || eventNameMatchesAnyPrefix(name, eventNamePrefixes);
    if (!durable || !offered) continue;
    const group = familyLabel(family);
    if (!groups.has(group)) groups.set(group, { group, items: [] });
    groups.get(group)!.items.push({ value: name, label: rest.length ? readableWords(rest) : familyLabel(name) });
  }
  return Array.from(groups.values());
}
