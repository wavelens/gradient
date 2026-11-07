/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { fieldChecks, firstFieldError } from './validators';

describe('fieldChecks', () => {
  it('leaves empty values to the required decision', () => {
    for (const check of Object.values(fieldChecks)) {
      expect(check('')).toBeNull();
      expect(check(null)).toBeNull();
    }
  });

  it.each([
    ['positiveInteger', ['1', 30, ' 7 '], ['0', '-5', '1.5', 'abc']],
    ['uuid', ['123e4567-e89b-12d3-a456-426614174000'], ['123', 'not-a-uuid']],
    ['base64', ['aGVsbG8=', 'aGVsbG8h'], ['not base64!', 'abc']],
    ['httpUrl', ['https://example.com/hook', 'http://10.0.0.1:8080'], ['not-a-url', 'ftp://example.com', 'example.com']],
    ['remoteRepository', ['https://github.com/acme/repo', 'git@github.com:acme/repo.git'], ['file:///srv/repo', '/srv/repo', './repo', '~/repo']],
  ] as const)('%s accepts good values and names the problem for bad ones', (name, good, bad) => {
    const check = fieldChecks[name];
    for (const value of good) expect(check(value)).toBeNull();
    for (const value of bad) expect(check(value)).toEqual(expect.any(String));
  });

  it('reports the first failing check only', () => {
    expect(firstFieldError('0', fieldChecks.positiveInteger, fieldChecks.uuid)).toBe(fieldChecks.positiveInteger('0'));
    expect(firstFieldError('5', fieldChecks.positiveInteger)).toBeNull();
  });
});
