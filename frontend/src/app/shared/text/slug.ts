/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

const GERMAN_SPELLING: Record<string, string> = { ä: 'ae', ö: 'oe', ü: 'ue', ß: 'ss' };

// German letters get their written-out spelling ("Köln" -> "koeln"); any other
// accent is dropped ("Café" -> "cafe") before the rest collapses to hyphens.
export function slugify(text: string): string {
  return text
    .normalize('NFC')
    .toLowerCase()
    .replace(/[äöüß]/g, (letter) => GERMAN_SPELLING[letter])
    .normalize('NFKD')
    .replace(/\p{M}/gu, '')
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '');
}
