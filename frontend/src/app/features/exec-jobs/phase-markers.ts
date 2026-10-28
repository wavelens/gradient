/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

const PHASE_MARKER = /^::gradient-phase:: (.+?)\r?$/;

export function phaseMarker(line: string): string | null {
  return PHASE_MARKER.exec(line)?.[1] ?? null;
}
