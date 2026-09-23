/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

const HOUR = 3_600_000;
const YEAR = 365 * 24 * HOUR;
const BAR_PITCH_PX = 7;
const MAX_BARS = 60;

export function formatCpuTime(ms: number): string {
  return ms >= YEAR ? `${(ms / YEAR).toFixed(1)} y` : `${Math.round(ms / HOUR)} h`;
}

export function barsThatFit(width: number): number {
  return Math.min(MAX_BARS, Math.max(1, Math.floor(width / BAR_PITCH_PX)));
}
