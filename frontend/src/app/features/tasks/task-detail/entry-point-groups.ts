/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { EntryPointSummary, FailedAttributeSummary } from '@core/models/task.model';

type Row =
  | { kind: 'build'; key: string; entry: EntryPointSummary }
  | { kind: 'failed'; key: string; failure: FailedAttributeSummary };

export type EntryPointRow = Row & { label: string };

interface Member {
  row: Row;
  attr: string;
  path: string[];
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

function isOtherSet(set: string): boolean {
  return /^others?$/i.test(set);
}

function pathWithinSet(attr: string, architecture?: string): { set: string; path: string[] } {
  const [set, ...rest] = attrSegments(attr);
  if (!rest.length) return { set, path: [set] };
  const path = rest.length > 1 && rest[0] === architecture ? rest.slice(1) : rest;
  return { set, path };
}

function sharedTailLength(paths: string[][]): number {
  const shortest = Math.min(...paths.map((p) => p.length));
  let tail = 0;
  while (tail < shortest - 1 && paths.every((p) => p.at(-1 - tail) === paths[0].at(-1 - tail))) tail++;
  return tail;
}

function compareAttr(a: string, b: string): number {
  return a < b ? -1 : a > b ? 1 : 0;
}

// Rows load page by page in attribute order, so a failure behind the last loaded row waits for its page.
export function failuresWithinLoadedRows(
  failed: FailedAttributeSummary[],
  loaded: EntryPointSummary[],
  total: number,
): FailedAttributeSummary[] {
  if (loaded.length >= total) return failed;
  const last = loaded.at(-1);
  return last ? failed.filter((f) => compareAttr(f.eval, last.eval) < 0) : [];
}

export function groupEntryPoints(
  entryPoints: EntryPointSummary[],
  failed: FailedAttributeSummary[] = [],
): EntryPointGroup[] {
  const sets = new Map<string, Member[]>();
  const add = (attr: string, architecture: string | undefined, row: Row) => {
    const { set, path } = pathWithinSet(attr, architecture);
    const members = sets.get(set);
    if (members) members.push({ row, attr, path });
    else sets.set(set, [{ row, attr, path }]);
  };
  for (const entry of entryPoints) add(entry.eval, entry.architecture, { kind: 'build', key: entry.id, entry });
  for (const failure of failed) add(failure.eval, undefined, { kind: 'failed', key: `failed:${failure.eval}`, failure });

  return [...sets].sort(([a], [b]) => Number(isOtherSet(a)) - Number(isOtherSet(b))).map(([set, members]) => {
    if (failed.length) members.sort((a, b) => compareAttr(a.attr, b.attr));
    const tail = sharedTail(members);
    return {
      title: headingOf(set),
      rows: members.map(({ row, path }) => ({ ...row, label: withoutTail(path, tail).join('.') })),
    };
  });
}

// A failure can stop partway down the path, so built rows alone decide the shared tail.
function sharedTail(members: Member[]): string[] {
  const built = members.filter((m) => m.row.kind === 'build');
  const basis = built.length > 1 ? built : members;
  if (basis.length < 2) return [];
  const length = sharedTailLength(basis.map((m) => m.path));
  return length ? basis[0].path.slice(-length) : [];
}

function withoutTail(path: string[], tail: string[]): string[] {
  const start = path.length - tail.length;
  const ends = tail.length > 0 && start > 0 && !tail.some((seg, i) => path[start + i] !== seg);
  return ends ? path.slice(0, start) : path;
}
