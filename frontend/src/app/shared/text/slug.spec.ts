/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { slugify } from './slug';

describe('slugify', () => {
  it('transliterates German umlauts instead of dropping them', () => {
    expect(slugify('NüschtOS')).toBe('nuschtos');
    expect(slugify('Übersicht')).toBe('ubersicht');
    expect(slugify('Öl Ärger')).toBe('ol-arger');
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
    expect(slugify('My  Cool__Project!!')).toBe('my-cool-project');
    expect(slugify('  spaced  ')).toBe('spaced');
  });

  it('returns an empty string when nothing slug-worthy remains', () => {
    expect(slugify('')).toBe('');
    expect(slugify('—')).toBe('');
  });
});
