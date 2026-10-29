/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { compareVersions, releaseLine } from './version';

describe('releaseLine', () => {
  it('drops the patch, the release candidate suffix and build metadata', () => {
    expect(releaseLine('2.1.3')).toBe('2.1');
    expect(releaseLine('2.1.0-rc2+abc')).toBe('2.1');
    expect(releaseLine('3')).toBe('3.0');
  });
});

describe('compareVersions', () => {
  it('orders release parts numerically', () => {
    expect(compareVersions('2.10.0', '2.9.3')).toBe(1);
    expect(compareVersions('2.0.1', '2.0.1')).toBe(0);
    expect(compareVersions('2.0', '2.0.1')).toBe(-1);
  });

  it('puts a release candidate before its release and ignores build metadata', () => {
    expect(compareVersions('2.1.0-rc2', '2.1.0')).toBe(-1);
    expect(compareVersions('2.1.0-rc10', '2.1.0-rc2')).toBe(1);
    expect(compareVersions('2.1.0+abc', '2.1.0')).toBe(0);
  });
});
