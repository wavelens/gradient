/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { RecapSlideColorSchemeName } from './yearly-recap-slides';

export interface YearlyRecapSlideColors {
  background: string;
  text: string;
  accent: string;
  bands: readonly [string, string, string];
}

export const deEmphasizedSmallTextOpacity = 0.8;

export const recapSlideColorSchemeColors: Record<RecapSlideColorSchemeName, YearlyRecapSlideColors> = {
  intro: { background: '#0f3d2e', text: '#eafff3', accent: '#7cf5b0', bands: ['#1d6b4c', '#3fb27f', '#c6ff8f'] },
  totals: { background: '#2b1a66', text: '#f4efff', accent: '#ffb3e6', bands: ['#4b2fb0', '#8a5cff', '#ff7ac8'] },
  mostBuilt: { background: '#d4ff4f', text: '#132a10', accent: '#2c6b1c', bands: ['#a8eb2a', '#7dd321', '#f4ffbf'] },
  records: { background: '#a01039', text: '#fff5f7', accent: '#ffdce5', bands: ['#ff5c82', '#6e0a26', '#ff9fb6'] },
  cache: { background: '#93d5ff', text: '#06233f', accent: '#0c4c8a', bands: ['#5bb7f5', '#d9f0ff', '#2a86d1'] },
  days: { background: '#ffb20f', text: '#2c1700', accent: '#663000', bands: ['#ff8a00', '#ffd56b', '#fff2c7'] },
  workers: { background: '#0c4d5a', text: '#e2fffa', accent: '#5df2da', bands: ['#13707f', '#2cb5a8', '#9dfbe9'] },
  summary: { background: '#eafff3', text: '#0f3d2e', accent: '#1d6b4c', bands: ['#7cf5b0', '#c6ff8f', '#93d5ff'] },
};
