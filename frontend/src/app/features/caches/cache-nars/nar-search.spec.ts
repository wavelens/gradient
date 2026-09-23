/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { narSearchText, parseNarSearch } from './nar-search';

const HASH = '0c3kv3g4wbs3f7rm2kfl7y0ybd4c1d2x';

describe('parseNarSearch', () => {
  it('reads a full store path as hash and package', () => {
    expect(parseNarSearch(`/nix/store/${HASH}-hello-2.12.1`)).toEqual({ hash: HASH, package: 'hello-2.12.1' });
  });

  it('ignores a path below the store entry', () => {
    expect(parseNarSearch(`/nix/store/${HASH}-hello-2.12.1/bin/hello`)).toEqual({ hash: HASH, package: 'hello-2.12.1' });
  });

  it('reads a store entry without the store dir', () => {
    expect(parseNarSearch(` ${HASH}-glibc-2.40.drv `)).toEqual({ hash: HASH, package: 'glibc-2.40.drv' });
  });

  it('reads a bare hash', () => {
    expect(parseNarSearch(HASH.toUpperCase())).toEqual({ hash: HASH });
  });

  it('reads a hash prefix that carries a digit', () => {
    expect(parseNarSearch('0c3kv3g4')).toEqual({ hash: '0c3kv3g4' });
  });

  it('treats everything else as a package name', () => {
    expect(parseNarSearch('glibc')).toEqual({ package: 'glibc' });
    expect(parseNarSearch('python3-3.12')).toEqual({ package: 'python3-3.12' });
    expect(parseNarSearch('hello')).toEqual({ package: 'hello' });
  });

  it('returns no filter for blank input', () => {
    expect(parseNarSearch('   ')).toEqual({});
  });
});

describe('narSearchText', () => {
  it('round-trips a parsed query', () => {
    for (const text of [`${HASH}-hello`, HASH, 'glibc', '']) {
      expect(narSearchText(parseNarSearch(text))).toBe(text);
    }
  });
});
