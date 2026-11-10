/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

// ANSI colors map to color roles of the theme. Log lines remain readable in light and dark themes. Bright variants share the same role as normal colors.
const COLOUR_ROLES = ['text-primary', 'status-danger', 'status-success', 'status-warning', 'status-info', 'log-magenta', 'log-cyan', 'text-secondary'];
const BRIGHT_ROLES = ['text-muted', ...COLOUR_ROLES.slice(1, 7), 'text-primary'];

interface CurrentTextStyle {
  bold: boolean;
  dim: boolean;
  italic: boolean;
  underline: boolean;
  foreground: string | null;
  background: string | null;
}

const DEFAULT_STYLE: CurrentTextStyle = { bold: false, dim: false, italic: false, underline: false, foreground: null, background: null };

// eslint-disable-next-line no-control-regex -- ESC and BEL delimit terminal escape sequences
export const anyTerminalEscapeSequencePattern = /\u001b(?:\[[0-?]*[ -/]*[@-~]|\][^\u0007\u001b]*(?:\u0007|\u001b\\)?|[@-Z\\-_])/g;
const SGR_PARAMETERS = /^\[([0-9;]*)m$/;

const MATCH_MARK = '<mark style="background:color-mix(in srgb, var(--gr-status-warning) 45%, transparent);color:inherit">';

const role = (name: string): string => `var(--gr-${name})`;
const isByte = (value: number): boolean => Number.isInteger(value) && value >= 0 && value <= 255;

function xtermPaletteIndexToCssColor(index: number | undefined): string | null {
  if (index === undefined || !isByte(index)) return null;
  if (index < 8) return role(COLOUR_ROLES[index]);
  if (index < 16) return role(BRIGHT_ROLES[index - 8]);
  if (index >= 232) {
    const grey = 8 + (index - 232) * 10;
    return `rgb(${grey},${grey},${grey})`;
  }
  const cube = index - 16;
  const level = (step: number) => (step === 0 ? 0 : 55 + step * 40);
  return `rgb(${level(Math.floor(cube / 36))},${level(Math.floor(cube / 6) % 6)},${level(cube % 6)})`;
}

function applySgrParametersToTextStyle(style: CurrentTextStyle, parameters: string): CurrentTextStyle {
  const codes = parameters.split(';').map((code) => (code === '' ? 0 : Number(code)));
  let next = { ...style };
  for (let i = 0; i < codes.length; i++) {
    const code = codes[i];
    if (code === 0) next = { ...DEFAULT_STYLE };
    else if (code === 1) next.bold = true;
    else if (code === 2) next.dim = true;
    else if (code === 3) next.italic = true;
    else if (code === 4) next.underline = true;
    else if (code === 22) next = { ...next, bold: false, dim: false };
    else if (code === 23) next.italic = false;
    else if (code === 24) next.underline = false;
    else if (code >= 30 && code <= 37) next.foreground = role(COLOUR_ROLES[code - 30]);
    else if (code >= 90 && code <= 97) next.foreground = role(BRIGHT_ROLES[code - 90]);
    else if (code === 39) next.foreground = null;
    else if (code >= 40 && code <= 47) next.background = role(COLOUR_ROLES[code - 40]);
    else if (code >= 100 && code <= 107) next.background = role(BRIGHT_ROLES[code - 100]);
    else if (code === 49) next.background = null;
    else if (code === 38 || code === 48) {
      const mode = codes[i + 1];
      const values = codes.slice(i + 2, i + 2 + (mode === 5 ? 1 : mode === 2 ? 3 : 0));
      i += 1 + values.length;
      const colour = mode === 5
        ? xtermPaletteIndexToCssColor(values[0])
        : mode === 2 && values.length === 3 && values.every(isByte) ? `rgb(${values.join(',')})` : null;
      if (colour) next[code === 38 ? 'foreground' : 'background'] = colour;
    }
  }
  return next;
}

function textStyleToCssDeclarations(style: CurrentTextStyle): string {
  return [
    style.foreground && `color:${style.foreground}`,
    style.background && `background:color-mix(in srgb, ${style.background} 30%, transparent)`,
    style.bold && 'font-weight:bold',
    style.dim && 'opacity:0.6',
    style.italic && 'font-style:italic',
    style.underline && 'text-decoration:underline',
  ].filter(Boolean).join(';');
}

function escapeHtmlAmpersandLessThanGreaterThan(text: string): string {
  return text.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
}

function findCaseInsensitiveMatchRanges(plain: string, query: string): [number, number][] {
  const haystack = plain.toLowerCase();
  const needle = query.toLowerCase();
  const ranges: [number, number][] = [];
  for (let at = haystack.indexOf(needle); needle && at >= 0; at = haystack.indexOf(needle, at + needle.length)) {
    ranges.push([at, at + needle.length]);
  }
  return ranges;
}

function escapePlainTextAndWrapSearchMatchesInMarkTags(text: string, offset: number, ranges: readonly [number, number][]): string {
  let out = '';
  let at = 0;
  for (const [start, end] of ranges) {
    const from = Math.max(start - offset, at);
    const to = Math.min(end - offset, text.length);
    if (to <= from) continue;
    out += escapeHtmlAmpersandLessThanGreaterThan(text.slice(at, from)) + MATCH_MARK + escapeHtmlAmpersandLessThanGreaterThan(text.slice(from, to)) + '</mark>';
    at = to;
  }
  return out + escapeHtmlAmpersandLessThanGreaterThan(text.slice(at));
}

export function ansiToHtml(text: string, highlightedQuery = ''): string {
  const runs: { text: string; css: string }[] = [];
  let style = DEFAULT_STYLE;
  let last = 0;
  for (const match of text.matchAll(anyTerminalEscapeSequencePattern)) {
    if (match.index > last) runs.push({ text: text.slice(last, match.index), css: textStyleToCssDeclarations(style) });
    const sgr = SGR_PARAMETERS.exec(match[0].slice(1));
    if (sgr) style = applySgrParametersToTextStyle(style, sgr[1]);
    last = match.index + match[0].length;
  }
  if (last < text.length) runs.push({ text: text.slice(last), css: textStyleToCssDeclarations(style) });

  const ranges = highlightedQuery ? findCaseInsensitiveMatchRanges(runs.map((run) => run.text).join(''), highlightedQuery) : [];
  let position = 0;
  return runs.map((run) => {
    const body = escapePlainTextAndWrapSearchMatchesInMarkTags(run.text, position, ranges);
    position += run.text.length;
    return run.css ? `<span style="${run.css}">${body}</span>` : body;
  }).join('');
}
