/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

export type LogSegment = { kind: 'text'; text: string } | { kind: 'phase'; name: string };

const PHASE_MARKER = /^::gradient-phase:: (.+?)\r?\n?$/;

function linesWithEndings(log: string): string[] {
  return log.match(/[^\n]*\n|[^\n]+$/g) ?? [];
}

export function splitPhaseMarkers(log: string): LogSegment[] {
  const segments: LogSegment[] = [];
  let text = '';
  const flushText = () => {
    if (text) segments.push({ kind: 'text', text });
    text = '';
  };
  for (const line of linesWithEndings(log)) {
    const marker = PHASE_MARKER.exec(line);
    if (marker) {
      flushText();
      segments.push({ kind: 'phase', name: marker[1] });
    } else {
      text += line;
    }
  }
  flushText();
  return segments;
}
