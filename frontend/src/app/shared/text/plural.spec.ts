/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { plural } from './plural';

describe('plural', () => {
  it('counts one in the singular and every other count in the plural', () => {
    expect(plural(1, 'package')).toBe('1 package');
    expect(plural(0, 'package')).toBe('0 packages');
    expect(plural(1, 'dependency', 'dependencies')).toBe('1 dependency');
    expect(plural(3, 'dependency', 'dependencies')).toBe('3 dependencies');
  });
});
