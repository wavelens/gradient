/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { UserInfoResponse, TeamSummary } from '@core/models';

export function canOpenTeam(user: UserInfoResponse | null | undefined, memberOf: readonly TeamSummary[], team: string): boolean {
  return user?.superuser === true || memberOf.some((mine) => mine.name === team);
}
