/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Signal, inject } from '@angular/core';
import { toSignal } from '@angular/core/rxjs-interop';
import { catchError, of } from 'rxjs';
import { AuthService } from '@core/services/auth.service';
import { UserService } from '@core/services/user.service';
import type { TeamNameAndDisplayName, UserAccessSummary } from '@core/models';

export function loadSignedInUserAccessSummary(): Signal<UserAccessSummary | null> {
  const auth = inject(AuthService);
  const users = inject(UserService);
  const summary = auth.isAuthenticated()
    ? users.getProjectsCachesAndTeamsCurrentUserCanReach().pipe(catchError(() => of(null)))
    : of(null);
  return toSignal(summary, { initialValue: null });
}

export function getDisplayNameWithOwnerTeam(displayName: string, ownerTeam: TeamNameAndDisplayName | null | undefined): string {
  return ownerTeam ? `${displayName} · ${ownerTeam.display_name || ownerTeam.name}` : displayName;
}
