/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { inject } from '@angular/core';
import { EMPTY, Observable, catchError } from 'rxjs';
import { NotifyService } from '@core/services/notify.service';
import { ConfirmService } from '@shared/confirm';

type MemberPlace = 'project' | 'cache' | 'team';

export interface OwnRoleChangeDialogInput<T> {
  place: MemberPlace;
  signedInUser: boolean;
  lowersAccess: boolean;
  target: string;
  save: () => Observable<T>;
}

export function newRoleLacksCurrentRolePermission(
  roles: readonly { name: string; permissions: readonly string[] }[],
  current: string,
  next: string,
): boolean {
  const permissionsOf = (name: string) => roles.find((role) => role.name === name)?.permissions ?? [];
  const nextPermissions = permissionsOf(next);
  return permissionsOf(current).some((permission) => !nextPermissions.includes(permission));
}

export function getMemberRemovalConsequenceSentence(place: MemberPlace, username: string, signedInUser: boolean): string {
  return signedInUser ? `You lose your role on this ${place}.` : `${username} loses their role on this ${place}.`;
}

export function injectSaveMemberRoleChangeWithOwnRoleConfirmation(): <T>(change: OwnRoleChangeDialogInput<T>) => Observable<T> {
  const confirm = inject(ConfirmService);
  const notify = inject(NotifyService);
  return <T>(change: OwnRoleChangeDialogInput<T>): Observable<T> => {
    if (change.signedInUser && change.lowersAccess) {
      return confirm.confirm({
        title: 'Change role',
        target: change.target,
        consequence: `You lose some permissions in this ${change.place}.`,
        confirmLabel: 'Change role',
        failure: "Couldn't change member role",
        severity: 'danger',
        action: change.save,
      });
    }
    return change.save().pipe(
      catchError((error: unknown) => {
        notify.failure("Couldn't change the member's role", error);
        return EMPTY;
      }),
    );
  };
}
