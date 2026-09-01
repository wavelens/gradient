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
  ['--gr-text-primary', '--gr-surface-control', 4.5],
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
  ['--gr-status-success-fg', '--gr-status-success', 4.5],
];

/// Elevation is only readable if adjacent surfaces actually differ.
const SURFACES = ['--gr-surface-sunken', '--gr-surface-base', '--gr-surface-raised', '--gr-surface-hover', '--gr-surface-active'];

/// Badges and banners composite the status colour over a surface at a low alpha,
/// so the shipped background is never the raw token. Model that here.
function mix(fg: string, bg: string, pct: number): string {
  const parse = (h: string) => [1, 3, 5].map((i) => parseInt(h.slice(i, i + 2), 16));
  const [f, b] = [parse(fg), parse(bg)];
  const out = f.map((c, i) => Math.round((c * pct + b[i] * (100 - pct)) / 100));
  return '#' + out.map((c) => c.toString(16).padStart(2, '0')).join('');
}

const TINTED: Array<[string, number]> = [
  ['--gr-status-success', 18],
  ['--gr-status-danger', 18],
  ['--gr-status-warning', 18],
  ['--gr-status-info', 18],
];

describe.each(['dark', 'light'] as Theme[])('%s theme tinted badges', (theme) => {
  it.each(TINTED)('%s label reads on its own %s%% tint', (role, pct) => {
    const tint = mix(resolveRole(role, theme), resolveRole('--gr-surface-base', theme), pct);
    expect(ratio(resolveRole(`${role}-text`, theme), tint)).toBeGreaterThanOrEqual(4.5);
  });
});

describe.each(['dark', 'light'] as Theme[])('%s theme surfaces', (theme) => {
  it('gives every surface role a distinct value', () => {
    const values = SURFACES.map((r) => resolveRole(r, theme));
    expect(new Set(values).size, values.join(' ')).toBe(SURFACES.length);
  });

  it('defines a control against the card it sits in, by fill or by border', () => {
    const control = resolveRole('--gr-surface-control', theme);
    const card = resolveRole('--gr-surface-raised', theme);
    const border = resolveRole('--gr-border', theme);
    const byFill = ratio(control, card);
    const byBorder = ratio(border, control);
    expect(Math.max(byFill, byBorder), `fill ${byFill.toFixed(2)} border ${byBorder.toFixed(2)}`).toBeGreaterThanOrEqual(3);
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
