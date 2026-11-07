/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

// ANSI colors map to color roles of the theme. Log lines remain readable in light and dark themes. Bright variants share the same role as normal colors.
const COLOUR_ROLES: Record<string, string> = {
  '30': 'text-primary',
  '31': 'status-danger',
  '32': 'status-success',
  '33': 'status-warning',
  '34': 'status-info',
  '35': 'log-magenta',
  '36': 'log-cyan',
  '37': 'text-secondary',
};
const BRIGHT_ROLES: Record<string, string> = { '90': 'text-muted', '97': 'text-primary' };

const SGR_SPANS: Record<string, string> = {
  '1': '<span style="font-weight:bold">',
  '2': '<span style="opacity:0.6">',
  '3': '<span style="font-style:italic">',
  ...Object.fromEntries(
    Object.entries(COLOUR_ROLES).flatMap(([code, role]) => {
      const colour = `color:var(--gr-${role})`;
      const bold = `<span style="${colour};font-weight:bold">`;
      return [
        [code, `<span style="${colour}">`],
        [String(Number(code) + 60), `<span style="${colour}">`],
        [`1;${code}`, bold],
        [`${code};1`, bold],
      ];
    }),
  ),
  ...Object.fromEntries(Object.entries(BRIGHT_ROLES).map(([code, role]) => [code, `<span style="color:var(--gr-${role})">`])),
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
