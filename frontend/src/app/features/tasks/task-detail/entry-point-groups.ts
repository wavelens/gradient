/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { EntryPointSummary } from '@core/models/task.model';

export interface EntryPointRow {
  entry: EntryPointSummary;
  label: string;
}

export interface EntryPointGroup {
  title: string;
  rows: EntryPointRow[];
}

// Segments are dot-separated outside quotes, so `pkgs."x.y"` ends at `x.y`.
function attrSegments(attr: string): string[] {
  const parts = attr.match(/"[^"]*"|[^."]+/g) ?? [];
  const segments = parts.map((s) => s.replace(/^"|"$/g, '')).filter((s) => s.length > 0);
  return segments.length ? segments : [attr];
}

function headingOf(set: string): string {
  const words = set.replace(/([a-z0-9])([A-Z])/g, '$1 $2');
  return words.charAt(0).toUpperCase() + words.slice(1);
}

function pathWithinSet(entry: EntryPointSummary): { set: string; path: string[] } {
  const [set, ...rest] = attrSegments(entry.eval);
  if (!rest.length) return { set, path: [set] };
  const path = rest.length > 1 && rest[0] === entry.architecture ? rest.slice(1) : rest;
  return { set, path };
}

function sharedTailLength(paths: string[][]): number {
  const shortest = Math.min(...paths.map((p) => p.length));
  let tail = 0;
  while (tail < shortest - 1 && paths.every((p) => p.at(-1 - tail) === paths[0].at(-1 - tail))) tail++;
  return tail;
}

export function groupEntryPoints(entryPoints: EntryPointSummary[]): EntryPointGroup[] {
  const sets = new Map<string, { entry: EntryPointSummary; path: string[] }[]>();
  for (const entry of entryPoints) {
    const { set, path } = pathWithinSet(entry);
    sets.set(set, [...(sets.get(set) ?? []), { entry, path }]);
  }
  return [...sets].map(([set, members]) => {
    const tail = members.length > 1 ? sharedTailLength(members.map((m) => m.path)) : 0;
    return {
      title: headingOf(set),
      rows: members.map(({ entry, path }) => ({ entry, label: path.slice(0, path.length - tail).join('.') })),
    };
  });
}
