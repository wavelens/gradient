/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { slugify } from './slug';

describe('slugify', () => {
  it('spells out German umlauts with a trailing e', () => {
    expect(slugify('NüschtOS')).toBe('nueschtos');
    expect(slugify('Übersicht')).toBe('uebersicht');
    expect(slugify('Öl Ärger')).toBe('oel-aerger');
  });

  it('spells out umlauts typed as a base letter plus combining diaeresis', () => {
    expect(slugify('Ko\u0308ln')).toBe('koeln');
  });

  it('expands the sharp s to ss', () => {
    expect(slugify('Straße')).toBe('strasse');
    expect(slugify('GROẞ')).toBe('gross');
  });

  it('strips diacritics from other Latin scripts', () => {
    expect(slugify('Café Crème')).toBe('cafe-creme');
    expect(slugify('Señor')).toBe('senor');
  });

  it('lowercases, collapses separators to single hyphens and trims them', () => {
    expect(slugify('My  Cool__Task!!')).toBe('my-cool-task');
    expect(slugify('  spaced  ')).toBe('spaced');
  });

  it('returns an empty string when nothing slug-worthy remains', () => {
    expect(slugify('')).toBe('');
    expect(slugify('—')).toBe('');
  });
});
