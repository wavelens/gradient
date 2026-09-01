/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

export interface RoleDoc {
  name: string;
  usage: string;
}

/// Layer 1. Raw values, theme-independent. Components never reference these directly.
export const PALETTE: Record<string, string> = {
  '--gr-white': '#ffffff',
  '--gr-black': '#000000',
  '--gr-gray-950': '#050708',
  '--gr-gray-900': '#0d1118',
  '--gr-gray-800': '#21262d',
  '--gr-gray-750': '#252d33',
  '--gr-gray-700': '#2d333b',
  '--gr-gray-400': '#818181',
  '--gr-gray-300': '#abb0b4',
  '--gr-gray-200': '#d0d7de',
  '--gr-gray-100': '#eaeef2',
  '--gr-gray-50': '#f6f8fa',
  '--gr-ink-900': '#1f2328',
  '--gr-slate-600': '#656d76',
  '--gr-slate-500': '#8c959f',
  '--gr-green-800': '#115c26',
  '--gr-green-700': '#166c2e',
  '--gr-green-600': '#1a7f37',
  '--gr-green-550': '#23913d',
  '--gr-green-300': '#34d399',
  '--gr-green-500': '#28a745',
  '--gr-green-400': '#22c55e',
  '--gr-red-700': '#a40e19',
  '--gr-red-600': '#cf222e',
  '--gr-red-550': '#bd2130',
  '--gr-red-500': '#dc3545',
  '--gr-red-400': '#ef4444',
  '--gr-amber-800': '#7d5300',
  '--gr-amber-700': '#9a6700',
  '--gr-amber-550': '#e6ac06',
  '--gr-amber-500': '#ffc107',
  '--gr-orange-500': '#f97316',
  '--gr-cyan-500': '#17a2b8',
  '--gr-blue-600': '#0969da',
  '--gr-blue-500': '#3b82f6',
  '--gr-purple-500': '#6f42c1',
};

/// Layer 2. Every value is a palette key, never a literal, so a theme is a pure remap.
export const DARK_ROLES: Record<string, string> = {
  '--gr-surface-sunken': '--gr-gray-950',
  '--gr-surface-base': '--gr-gray-900',
  '--gr-surface-raised': '--gr-gray-800',
  '--gr-surface-hover': '--gr-gray-750',
  '--gr-surface-active': '--gr-gray-800',
  '--gr-border': '--gr-gray-700',
  '--gr-accent': '--gr-green-300',
  '--gr-accent-hover': '--gr-green-300',
  '--gr-accent-fg': '--gr-gray-900',
  '--gr-text-primary': '--gr-white',
  '--gr-text-secondary': '--gr-gray-300',
  '--gr-text-muted': '--gr-gray-400',
  '--gr-status-success': '--gr-green-500',
  '--gr-status-danger': '--gr-red-500',
  '--gr-status-danger-hover': '--gr-red-550',
  '--gr-status-danger-fg': '--gr-white',
  '--gr-status-warning': '--gr-amber-500',
  '--gr-status-warning-hover': '--gr-amber-550',
  '--gr-status-warning-fg': '--gr-black',
  '--gr-status-info': '--gr-cyan-500',
  '--gr-graph-success': '--gr-green-400',
  '--gr-graph-danger': '--gr-red-400',
  '--gr-graph-warning': '--gr-orange-500',
  '--gr-graph-running': '--gr-blue-500',
};

export const LIGHT_ROLES: Record<string, string> = {
  '--gr-surface-sunken': '--gr-gray-50',
  '--gr-surface-base': '--gr-white',
  '--gr-surface-raised': '--gr-gray-50',
  '--gr-surface-hover': '--gr-gray-100',
  '--gr-surface-active': '--gr-gray-200',
  '--gr-border': '--gr-gray-200',
  '--gr-accent': '--gr-green-700',
  '--gr-accent-hover': '--gr-green-800',
  '--gr-accent-fg': '--gr-white',
  '--gr-text-primary': '--gr-ink-900',
  '--gr-text-secondary': '--gr-slate-600',
  '--gr-text-muted': '--gr-slate-500',
  '--gr-status-success': '--gr-green-600',
  '--gr-status-danger': '--gr-red-600',
  '--gr-status-danger-hover': '--gr-red-700',
  '--gr-status-danger-fg': '--gr-white',
  '--gr-status-warning': '--gr-amber-700',
  '--gr-status-warning-hover': '--gr-amber-800',
  '--gr-status-warning-fg': '--gr-white',
  '--gr-status-info': '--gr-blue-600',
  '--gr-graph-success': '--gr-green-600',
  '--gr-graph-danger': '--gr-red-600',
  '--gr-graph-warning': '--gr-orange-500',
  '--gr-graph-running': '--gr-blue-600',
};

export const SEMANTIC_ROLES: readonly RoleDoc[] = [
  { name: '--gr-surface-sunken', usage: 'Page background behind raised cards' },
  { name: '--gr-surface-base', usage: 'Default page background' },
  { name: '--gr-surface-raised', usage: 'Cards, dialogs, popovers' },
  { name: '--gr-surface-hover', usage: 'Hover row and control backgrounds' },
  { name: '--gr-surface-active', usage: 'Pressed and active control backgrounds' },
  { name: '--gr-border', usage: 'All borders and dividers' },
  { name: '--gr-accent', usage: 'Primary action colour: default buttons, checked controls, focus rings' },
  { name: '--gr-accent-hover', usage: 'Hover state for accent surfaces' },
  { name: '--gr-accent-fg', usage: 'Text and icons on an accent background' },
  { name: '--gr-text-primary', usage: 'Body and heading text' },
  { name: '--gr-text-secondary', usage: 'Supporting text, meta rows, hints' },
  { name: '--gr-text-muted', usage: 'Disabled and placeholder text' },
  { name: '--gr-status-success', usage: 'Success badges, completed states' },
  { name: '--gr-status-danger', usage: 'Errors, failed states, destructive actions' },
  { name: '--gr-status-danger-hover', usage: 'Hover state for destructive surfaces' },
  { name: '--gr-status-danger-fg', usage: 'Text and icons on a destructive background' },
  { name: '--gr-status-warning', usage: 'Warnings and degraded states' },
  { name: '--gr-status-warning-hover', usage: 'Hover state for warning surfaces' },
  { name: '--gr-status-warning-fg', usage: 'Text and icons on a warning background' },
  { name: '--gr-status-info', usage: 'Informational badges and links' },
  { name: '--gr-graph-success', usage: 'Chart series: successful builds' },
  { name: '--gr-graph-danger', usage: 'Chart series: failed builds' },
  { name: '--gr-graph-warning', usage: 'Chart series: warnings' },
  { name: '--gr-graph-running', usage: 'Chart series: running builds' },
] as const;

export function resolveRole(role: string, theme: 'dark' | 'light'): string {
  const roles = theme === 'dark' ? DARK_ROLES : LIGHT_ROLES;
  return PALETTE[roles[role]] ?? '';
}
