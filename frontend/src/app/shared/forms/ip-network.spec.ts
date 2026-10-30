/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { isIpNetwork, splitLines } from './ip-network';

describe('isIpNetwork', () => {
  it('accepts plain addresses and networks of both families', () => {
    for (const value of ['203.0.113.5', '10.0.0.0/8', '0.0.0.0/0', '2001:db8::1', '2001:db8::/32', '::1', 'fe80:0:0:0:0:0:0:1/64']) {
      expect(isIpNetwork(value), value).toBe(true);
    }
  });

  it('rejects host names, out-of-range octets and prefixes', () => {
    for (const value of ['example.com', '256.1.1.1', '10.0.0/8', '10.0.0.0/33', '2001:db8::/129', '1::2::3', '10.0.0.0/8/1', '10.0.0.0/']) {
      expect(isIpNetwork(value), value).toBe(false);
    }
  });
});

describe('splitLines', () => {
  it('drops blank lines and surrounding spaces', () => {
    expect(splitLines(' 10.0.0.0/8 \n\n::1\n')).toEqual(['10.0.0.0/8', '::1']);
  });
});
