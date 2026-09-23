/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

type Maybe = number | null | undefined;

const BYTE_UNITS = ['B', 'KiB', 'MiB', 'GiB', 'TiB', 'PiB'];

/// Three significant digits: `1.5 GiB`, `734 MiB`.
function significant(value: number): string {
  return value >= 100 ? Math.round(value).toString() : value.toFixed(1);
}

export function formatBytes(bytes: Maybe): string {
  if (bytes == null || !Number.isFinite(bytes)) return '-';
  if (Math.abs(bytes) < 1024) return `${Math.round(bytes)} B`;

  const exp = Math.min(BYTE_UNITS.length - 1, Math.floor(Math.log(Math.abs(bytes)) / Math.log(1024)));
  return `${significant(bytes / 1024 ** exp)} ${BYTE_UNITS[exp]}`;
}

export function formatMegabytes(mb: Maybe): string {
  return mb == null ? '-' : formatBytes(mb * 1024 * 1024);
}

const pad = (n: number) => n.toString().padStart(2, '0');

export function formatDuration(ms: Maybe): string {
  if (ms == null || !Number.isFinite(ms)) return '-';
  if (ms < 1000) return `${Math.round(ms)} ms`;
  const secs = ms / 1000;
  if (secs < 60) return `${secs.toFixed(1)} s`;
  const total = Math.floor(secs);
  const d = Math.floor(total / 86_400);
  const h = Math.floor((total % 86_400) / 3600);
  const m = Math.floor((total % 3600) / 60);
  if (d > 0) return `${d}d ${h}h`;
  if (h > 0) return `${h}h ${pad(m)}m`;
  return `${m}m ${pad(total % 60)}s`;
}

export function formatCount(n: Maybe): string {
  if (n == null || !Number.isFinite(n)) return '-';
  const abs = Math.abs(n);
  if (abs < 1000) return Math.round(n).toString();
  if (abs < 1_000_000) return `${(n / 1000).toFixed(1)}k`;
  if (abs < 1_000_000_000) return `${(n / 1_000_000).toPrecision(3)}M`;
  return `${(n / 1_000_000_000).toPrecision(3)}G`;
}

export function formatPercent(ratio: Maybe): string {
  return ratio == null || !Number.isFinite(ratio) ? '-' : `${(ratio * 100).toFixed(1)} %`;
}
