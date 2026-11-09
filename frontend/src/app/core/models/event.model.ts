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

const EVENT_LABELS: Record<string, string> = {
  'webhook.ping': 'Test delivery',
  'worker.demand': 'Worker required',
  'gc.swept': 'Cleanup pass complete',
  'gc.deep_finished': 'Full cleanup complete',
  'exec_job.phase': 'Exec job changed phase',
  'auth.deny': 'Access denied',
  'cli.device.start': 'CLI started sign-in',
  'cli.device.authorize': 'CLI sign-in approved',
  'cli.device.deny': 'CLI sign-in denied',
  'login.success': 'Signed in',
  'login.failure': 'Sign-in failure',
};

const PHRASES: Record<string, string> = {
  'member.add': 'member added',
  'member.create': 'member added',
  'member.remove': 'member removed',
  'member.delete': 'member removed',
  'member.role_change': 'member role changed',
  'member.update': 'member role changed',
  'grant.create': 'access granted',
  'grant.update': 'access changed',
  'grant.remove': 'access removed',
};

const WORDS: Record<string, string> = {
  failed_transient: 'failed, will retry',
  webhook: 'event notification',
  api: 'API',
  cli: 'CLI',
  gc: 'GC',
  nar: 'NAR',
  ssh: 'SSH',
  vm: 'VM',
};

const PAST_TENSE: Record<string, string> = {
  accept: 'accepted',
  approve: 'approved',
  authorize: 'authorized',
  cancel: 'cancelled',
  connect: 'connected',
  create: 'created',
  decline: 'declined',
  delete: 'deleted',
  deny: 'denied',
  disconnect: 'disconnected',
  request: 'requested',
  revoke: 'revoked',
  star: 'starred',
  start: 'started',
  unstar: 'unstarred',
  update: 'updated',
  upload: 'uploaded',
};

function words(segment: string): string {
  return WORDS[segment] ?? segment.split('_').map((word) => WORDS[word] ?? word).join(' ');
}

function phrase(segments: string[]): string {
  const tail = segments.slice(-2).join('.');
  if (PHRASES[tail]) return [...segments.slice(0, -2).map(words), PHRASES[tail]].join(' ');
  const last = segments[segments.length - 1];
  return [...segments.slice(0, -1).map(words), PAST_TENSE[last] ?? words(last)].join(' ');
}

function sentence(text: string): string {
  return text.charAt(0).toUpperCase() + text.slice(1);
}

/**
 * Readable text for a dot-separated event name. The name build.failed_transient can yield "Build failed, will retry".
 * - Wildcard names stay as they are.
 */
export function eventNameToReadableText(name: string): string {
  if (name.includes('*')) return name;
  return EVENT_LABELS[name] ?? sentence(phrase(name.split('.')));
}

export function eventNameMatchesAnyPrefix(name: string, eventNamePrefixes: readonly string[]): boolean {
  return eventNamePrefixes.some((prefix) => name === prefix || name.startsWith(`${prefix}.`));
}

/** Group durable events by their first dot-separated segment. If `eventNamePrefixes` is present, keep only names equal to the prefix or names that start with the prefix followed by a dot. */
export function groupCatalog(entries: CatalogEntry[], eventNamePrefixes?: readonly string[], sshEnabled = true): EventGroup[] {
  const groups = new Map<string, EventGroup>();
  for (const { name, durable } of entries) {
    const [family, ...rest] = name.split('.');
    const offered = (!eventNamePrefixes || eventNameMatchesAnyPrefix(name, eventNamePrefixes)) && (sshEnabled || family !== 'ssh_key');
    if (!durable || !offered) continue;
    const group = sentence(words(family));
    if (!groups.has(group)) groups.set(group, { group, items: [] });
    groups.get(group)!.items.push({ value: name, label: EVENT_LABELS[name] ?? sentence(rest.length ? phrase(rest) : words(name)) });
  }
  return Array.from(groups.values());
}
