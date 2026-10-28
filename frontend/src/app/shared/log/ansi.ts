/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

const SGR_SPANS: Record<string, string> = {
  '1': '<span style="font-weight:bold">',
  '2': '<span style="opacity:0.6">',
  '3': '<span style="font-style:italic">',
  '30': '<span style="color:#374151">',
  '31': '<span style="color:#ef4444">',
  '32': '<span style="color:#22c55e">',
  '33': '<span style="color:#eab308">',
  '34': '<span style="color:#3b82f6">',
  '35': '<span style="color:#a855f7">',
  '36': '<span style="color:#06b6d4">',
  '37': '<span style="color:#d1d5db">',
  '90': '<span style="color:#6b7280">',
  '91': '<span style="color:#f87171">',
  '92': '<span style="color:#4ade80">',
  '93': '<span style="color:#fbbf24">',
  '94': '<span style="color:#60a5fa">',
  '95': '<span style="color:#c084fc">',
  '96': '<span style="color:#22d3ee">',
  '97': '<span style="color:#f9fafb">',
  '1;31': '<span style="color:#ef4444;font-weight:bold">',
  '1;32': '<span style="color:#22c55e;font-weight:bold">',
  '1;33': '<span style="color:#eab308;font-weight:bold">',
  '1;34': '<span style="color:#3b82f6;font-weight:bold">',
  '1;35': '<span style="color:#a855f7;font-weight:bold">',
  '1;36': '<span style="color:#06b6d4;font-weight:bold">',
  '31;1': '<span style="color:#ef4444;font-weight:bold">',
  '35;1': '<span style="color:#a855f7;font-weight:bold">',
};

const SGR_RESETS = new Set(['0', '', '39', '49', '22', '23', '24']);

// eslint-disable-next-line no-control-regex -- ESC is the sequence introducer
const CSI_SEQUENCE = /\u001b\[([?0-9;]*)([A-Za-z~])/g;

export function ansiToHtml(text: string): string {
  const escaped = text
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;');
  let openSpans = 0;
  const html = escaped.replace(CSI_SEQUENCE, (_, params: string, command: string) => {
    if (command !== 'm') return '';
    if (SGR_RESETS.has(params)) {
      const closing = '</span>'.repeat(openSpans);
      openSpans = 0;
      return closing;
    }
    const span = SGR_SPANS[params];
    if (!span) return '';
    openSpans++;
    return span;
  });
  return html + '</span>'.repeat(openSpans);
}
