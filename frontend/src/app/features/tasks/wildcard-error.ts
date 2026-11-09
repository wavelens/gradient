/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

const BARE_SPECIAL = "A pattern part can't be only `!`, `*` or `**` in quotes and `!` only starts a pattern.";

/// Mirrors `Wildcard::from_str` in `backend/gradient-types/src/wildcard.rs`.
export function wildcardError(wildcard: string): string | null {
  if (wildcard === '') return 'Enter a wildcard.';
  if (wildcard.trim() !== wildcard) return 'Remove the spaces around the wildcard.';
  if (wildcard.includes(',,')) return 'Remove the empty pattern between two commas.';
  for (const part of wildcard.split(',').map((p) => p.trim())) {
    const error = patternError(part);
    if (error) return error;
  }
  return null;
}

function patternError(part: string): string | null {
  if (part === '') return 'Remove the empty pattern between two commas.';
  if (/\s/.test(part)) return 'Remove the spaces inside the pattern.';
  const exclusion = part.startsWith('!');
  const body = exclusion ? part.slice(1) : part;
  if (body === '') return BARE_SPECIAL;
  if (body.startsWith('.')) return "A pattern can't start with a period.";
  const segments = splitSegments(body);
  if (exclusion && segments.some((s) => !s.quoted && s.text.includes('*'))) {
    return "Exclusion patterns (starting with `!`) can't contain `*` or `**`.";
  }
  for (const { text, quoted } of segments) {
    if (quoted ? ['*', '**', '#', '!'].includes(text.replace(/^"+|"+$/g, '')) : text.startsWith('!')) return BARE_SPECIAL;
    if (!quoted && text.includes('#')) return "Wildcards can't use `#`; use `*` for one level or `**` for nested outputs.";
  }
  return null;
}

function splitSegments(pattern: string): { text: string; quoted: boolean }[] {
  const segments: { text: string; quoted: boolean }[] = [];
  let text = '';
  let inQuotes = false;
  let quoted = false;
  for (const ch of pattern) {
    if (ch === '"') {
      inQuotes = !inQuotes;
      if (inQuotes) quoted = true;
      text += ch;
    } else if (ch === '.' && !inQuotes) {
      segments.push({ text, quoted });
      text = '';
      quoted = false;
    } else {
      text += ch;
    }
  }
  segments.push({ text, quoted });
  return segments;
}
