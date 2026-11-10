/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

export function buildMemberSoleAdminDisabledReasonTextForUsername(
  kind: 'project' | 'cache' | 'team',
  adminUsernames: readonly string[],
): (username: string) => string {
  const soleAdmin = adminUsernames.length === 1 ? adminUsernames[0] : null;
  return (username) =>
    username === soleAdmin ? `Set another member as Admin first. Every ${kind} needs at least one Admin.` : '';
}
