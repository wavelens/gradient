/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { plural } from '@shared/text';

const builtInRoleNamesByAccess = ['View', 'Write', 'Admin'];

export function compareRoleNamesByBuiltInAccessThenName(a: string, b: string): number {
  const rank = (name: string) => {
    const index = builtInRoleNamesByAccess.indexOf(name);
    return index === -1 ? builtInRoleNamesByAccess.length : index;
  };
  return rank(a) - rank(b) || a.localeCompare(b);
}

export interface RoleHolderCountResult {
  total: number;
  text: string;
}

export function countRoleHoldersAndBuildBreakdownText(
  roleNames: readonly string[],
  holders: {
    members: readonly { name: string }[];
    teamGrants: readonly { role?: string | null }[];
    invitations: readonly { role: string }[];
  },
): Record<string, RoleHolderCountResult> {
  const countOf = (roles: readonly (string | null | undefined)[], roleName: string) => roles.filter((role) => role === roleName).length;
  return Object.fromEntries(
    roleNames.map((roleName) => {
      const members = countOf(holders.members.map((member) => member.name), roleName);
      const teams = countOf(holders.teamGrants.map((grant) => grant.role), roleName);
      const invitations = countOf(holders.invitations.map((invitation) => invitation.role), roleName);
      const parts = [plural(members, 'member')];
      if (teams) parts.push(plural(teams, 'team'));
      if (invitations) parts.push(plural(invitations, 'invitation'));
      return [roleName, { total: members + teams + invitations, text: parts.join(', ') }];
    }),
  );
}
