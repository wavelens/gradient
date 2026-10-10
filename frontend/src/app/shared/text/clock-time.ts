/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

const pad = (n: number) => String(n).padStart(2, '0');

// The API sends `2026-09-01 11:00:00` without a zone; it means UTC.
export function serverTime(at: string | number): Date {
  if (typeof at === 'number') return new Date(at);
  const normalised = at.includes('T') ? at : at.replace(' ', 'T');

  return new Date(/(Z|[+-]\d{2}:?\d{2})$/.test(normalised) ? normalised : `${normalised}Z`);
}

export function clockTime(at: string | number): string {
  const time = serverTime(at);

  return `${pad(time.getHours())}:${pad(time.getMinutes())}`;
}

export function monthDay(at: string | number): string {
  const time = serverTime(at);

  return `${pad(time.getMonth() + 1)}-${pad(time.getDate())}`;
}
