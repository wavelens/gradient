/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { resolveRole } from './tokens';

type Theme = 'dark' | 'light';

function luminance(hex: string): number {
  const channels = [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16) / 255);
  const [r, g, b] = channels.map((c) => (c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4));
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

function ratio(a: string, b: string): number {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
}

const PAIRS: Array<[string, string, number]> = [
  ['--gr-text-primary', '--gr-surface-base', 4.5],
  ['--gr-text-primary', '--gr-surface-raised', 4.5],
  ['--gr-text-secondary', '--gr-surface-base', 4.5],
  ['--gr-text-secondary', '--gr-surface-raised', 4.5],
  ['--gr-status-success', '--gr-surface-base', 3],
  ['--gr-status-danger', '--gr-surface-base', 3],
  ['--gr-status-warning', '--gr-surface-base', 3],
  ['--gr-status-info', '--gr-surface-base', 3],
  ['--gr-accent', '--gr-surface-base', 3],
  ['--gr-accent-fg', '--gr-accent', 4.5],
  ['--gr-accent-fg', '--gr-accent-hover', 4.5],
  ['--gr-status-danger-fg', '--gr-status-danger', 4.5],
  ['--gr-status-warning-fg', '--gr-status-warning', 4.5],
  // Muted is real text, and every status colour is also used on a raised surface.
  ['--gr-text-muted', '--gr-surface-base', 4.5],
  ['--gr-text-muted', '--gr-surface-raised', 4.5],
  ['--gr-status-success', '--gr-surface-raised', 3],
  ['--gr-status-danger', '--gr-surface-raised', 3],
  ['--gr-status-warning', '--gr-surface-raised', 3],
  ['--gr-status-info', '--gr-surface-raised', 3],
  ['--gr-accent', '--gr-surface-raised', 3],
];

/// Elevation is only readable if adjacent surfaces actually differ.
const SURFACES = ['--gr-surface-sunken', '--gr-surface-base', '--gr-surface-raised', '--gr-surface-hover', '--gr-surface-active'];

describe.each(['dark', 'light'] as Theme[])('%s theme surfaces', (theme) => {
  it('gives every surface role a distinct value', () => {
    const values = SURFACES.map((r) => resolveRole(r, theme));
    expect(new Set(values).size, values.join(' ')).toBe(SURFACES.length);
  });

  it('keeps the border distinct from every surface', () => {
    const border = resolveRole('--gr-border', theme);
    for (const role of SURFACES) {
      expect(resolveRole(role, theme), `${role} equals the border`).not.toBe(border);
    }
  });
});

describe.each(['dark', 'light'] as Theme[])('%s theme contrast', (theme) => {
  it.each(PAIRS)('%s on %s meets %sx', (fg, bg, min) => {
    const foreground = resolveRole(fg, theme);
    const background = resolveRole(bg, theme);
    expect(foreground).toMatch(/^#[0-9a-f]{6}$/);
    expect(background).toMatch(/^#[0-9a-f]{6}$/);
    expect(ratio(foreground, background)).toBeGreaterThanOrEqual(min);
  });
});
