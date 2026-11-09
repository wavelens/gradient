/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { compareRoleNamesByBuiltInAccessThenName, countMembersByRoleName } from './role-order';

describe('role order', () => {
  it('lists built-in roles from lowest to highest access, then custom roles by name', () => {
    const names = ['releaser', 'Admin', 'auditor', 'View', 'Write'];
    expect(names.sort(compareRoleNamesByBuiltInAccessThenName)).toEqual(['View', 'Write', 'Admin', 'auditor', 'releaser']);
  });

  it('counts the members of each role', () => {
    const members = [{ name: 'View' }, { name: 'releaser' }, { name: 'View' }];
    expect(countMembersByRoleName(members)).toEqual({ View: 2, releaser: 1 });
  });
});
