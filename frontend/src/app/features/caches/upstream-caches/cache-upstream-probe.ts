/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

// Returns the entered substituter URL normalized to an absolute http(s) origin,
// or null when it is not a real absolute URL (so we never probe the app's own
// origin via a relative path).
export function normalizeProbeUrl(raw: string): string | null {
  const trimmed = raw.trim().replace(/\/+$/, '');
  if (!trimmed) return null;
  try {
    const { protocol } = new URL(trimmed);
    return protocol === 'http:' || protocol === 'https:' ? trimmed : null;
  } catch {
    return null;
  }
}

// True only when the parsed `gradient-cache-info?json` body carries the
// fields a Gradient server emits, so a plain 200 (e.g. an SPA fallback) is
// not mistaken for a Gradient cache.
export function isGradientCacheInfo(body: unknown): boolean {
  if (typeof body !== 'object' || body === null) return false;
  const info = body as Record<string, unknown>;
  return typeof info['GradientVersion'] === 'string' && typeof info['GradientUrl'] === 'string';
}
