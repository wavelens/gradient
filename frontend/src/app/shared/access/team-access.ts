/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { UserInfoResponse, TeamResponse, TeamSummary } from '@core/models';

export function canOpenTeam(user: UserInfoResponse | null | undefined, memberOf: readonly TeamSummary[], team: string): boolean {
  return user?.superuser === true || memberOf.some((mine) => mine.name === team);
}

export function canManageTeam(team: Pick<TeamResponse, 'role'> | null | undefined, user: UserInfoResponse | null | undefined): boolean {
  return team?.role === 'admin' || user?.superuser === true;
}
