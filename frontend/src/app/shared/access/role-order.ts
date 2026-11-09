/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

const builtInRoleNamesByAccess = ['View', 'Write', 'Admin'];

export function compareRoleNamesByBuiltInAccessThenName(a: string, b: string): number {
  const rank = (name: string) => {
    const index = builtInRoleNamesByAccess.indexOf(name);
    return index === -1 ? builtInRoleNamesByAccess.length : index;
  };
  return rank(a) - rank(b) || a.localeCompare(b);
}

export function countMembersByRoleName(members: readonly { name: string }[]): Record<string, number> {
  const counts: Record<string, number> = {};
  for (const member of members) counts[member.name] = (counts[member.name] ?? 0) + 1;
  return counts;
}
