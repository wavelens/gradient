/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { matchesBuildSearch } from './build-search';

describe('matchesBuildSearch', () => {
  it('empty query matches all', () => {
    expect(matchesBuildSearch('gcc-13.2.0', '')).toBe(true);
  });

  it('whitespace-only query matches all', () => {
    expect(matchesBuildSearch('gcc-13.2.0', '   ')).toBe(true);
  });

  it('case-insensitive substring match', () => {
    expect(matchesBuildSearch('gcc-13.2.0', 'GCC')).toBe(true);
    expect(matchesBuildSearch('nixos-system', 'system')).toBe(true);
  });

  it('non-matching query returns false', () => {
    expect(matchesBuildSearch('gcc-13.2.0', 'clang')).toBe(false);
  });
});
