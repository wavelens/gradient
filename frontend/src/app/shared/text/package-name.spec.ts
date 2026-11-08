/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { packageNameFromDerivationPath } from './package-name';

describe('packageNameFromDerivationPath', () => {
  it('drops the store directory, the hash and the .drv suffix', () => {
    expect(packageNameFromDerivationPath('/nix/store/abc123-hello-2.12.drv')).toBe('hello-2.12');
  });

  it('accepts a bare store file name', () => {
    expect(packageNameFromDerivationPath('abc123-zlib.drv')).toBe('zlib');
  });
});
