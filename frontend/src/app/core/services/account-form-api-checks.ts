/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ValidationErrors } from '@angular/forms';
import { Observable, catchError, map, of, switchMap, timer } from 'rxjs';
import { ApiError } from './api.service';
import { AuthService } from './auth.service';

export const apiErrorCodeToFormFieldName: Readonly<Record<string, 'username' | 'email'>> = {
  username_already_in_use: 'username',
  email_address_already_in_use: 'email',
};

export function checkUsernameAvailability(auth: AuthService, username: string): Observable<ValidationErrors | null> {
  if (!username) return of(null);
  return timer(500).pipe(
    switchMap(() => auth.checkUsername(username)),
    map(() => null),
    catchError((error: unknown) =>
      of(
        error instanceof ApiError && !error.unreachable && error.status < 500
          ? { usernameRefused: error.message }
          : { usernameCheckFailed: true },
      ),
    ),
  );
}
