/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

const MINUTE = 60;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;
const MONTH = 30 * DAY;
const YEAR = 365 * DAY;

/// How long ago, in the shortest form that still reads: "5m ago", "3d ago".
/// A metric has room for a short answer, not a full timestamp, so this is what
/// stat cards and row meta lines show.
export function relativeTime(iso: string | null | undefined, now = Date.now()): string {
  if (!iso) return 'never';

  // The API sends `2026-09-01 11:00:00` without a zone; it means UTC.
  const normalised = iso.includes('T') ? iso : iso.replace(' ', 'T');
  const withZone = /(Z|[+-]\d{2}:?\d{2})$/.test(normalised) ? normalised : `${normalised}Z`;
  const then = new Date(withZone).getTime();
  if (Number.isNaN(then)) return 'never';

  const secs = Math.floor((now - then) / 1000);
  if (secs < 0) return 'just now';
  if (secs < MINUTE) return `${secs}s ago`;
  if (secs < HOUR) return `${Math.floor(secs / MINUTE)}m ago`;
  if (secs < DAY) return `${Math.floor(secs / HOUR)}h ago`;
  if (secs < MONTH) return `${Math.floor(secs / DAY)}d ago`;
  if (secs < YEAR) return `${Math.floor(secs / MONTH)}mo ago`;
  return `${Math.floor(secs / YEAR)}y ago`;
}
