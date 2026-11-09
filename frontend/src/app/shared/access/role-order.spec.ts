/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { compareRoleNamesByBuiltInAccessThenName, countRoleHoldersAndBuildBreakdownText } from './role-order';
import { formatMemberNameForConfirmDialog } from './member-name';

describe('member and role helpers', () => {
  it('lists built-in roles from lowest to highest access, then custom roles by name', () => {
    const names = ['releaser', 'Admin', 'auditor', 'View', 'Write'];
    expect(names.sort(compareRoleNamesByBuiltInAccessThenName)).toEqual(['View', 'Write', 'Admin', 'auditor', 'releaser']);
  });

  it('counts members, team grants and invitations of each role', () => {
    const counts = countRoleHoldersAndBuildBreakdownText(['View', 'releaser', 'Admin'], {
      members: [{ name: 'View' }, { name: 'releaser' }, { name: 'View' }],
      teamGrants: [{ role: 'View' }, { role: null }],
      invitations: [{ role: 'View' }, { role: 'Admin' }],
    });
    expect(counts).toEqual({
      View: { total: 4, text: '2 members, 1 team, 1 invitation' },
      releaser: { total: 1, text: '1 member' },
      Admin: { total: 1, text: '0 members, 1 invitation' },
    });
  });

  it('names a member by display name and username, or by username alone', () => {
    expect(formatMemberNameForConfirmDialog('Carol Outsider', 'carol')).toBe('Carol Outsider (carol)');
    expect(formatMemberNameForConfirmDialog('carol', 'carol')).toBe('carol');
    expect(formatMemberNameForConfirmDialog(null, 'carol')).toBe('carol');
  });
});
