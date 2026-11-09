/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { wildcardError } from './wildcard-error';

describe('wildcardError', () => {
  it('accepts the patterns the server accepts', () => {
    for (const valid of ['packages.x86_64-linux.**', 'packages.*.hello, !packages.x86_64-linux.big', 'checks."x86_64-linux".*', '!a.b,c']) {
      expect(wildcardError(valid)).toBeNull();
    }
  });

  it('names the first broken rule like the server does', () => {
    expect(wildcardError('')).toBe('Enter a wildcard.');
    expect(wildcardError(' a.b')).toBe('Remove the spaces around the wildcard.');
    expect(wildcardError('a,,b')).toBe('Remove the empty pattern between two commas.');
    expect(wildcardError('a,')).toBe('Remove the empty pattern between two commas.');
    expect(wildcardError('a b')).toBe('Remove the spaces inside the pattern.');
    expect(wildcardError('.a')).toBe("A pattern can't start with a period.");
    expect(wildcardError('!')).toContain('`!`');
    expect(wildcardError('a."*"')).toContain('`!`');
    expect(wildcardError('a.!b')).toContain('`!`');
    expect(wildcardError('!a.*')).toContain('Exclusion patterns');
    expect(wildcardError('a.#')).toContain('`#`');
  });
});
