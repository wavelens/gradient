/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { inject } from '@angular/core';
import { Router, CanActivateFn } from '@angular/router';
import { map } from 'rxjs/operators';
import { AuthService } from '@core/services/auth.service';

/**
 * Route guard that allows access only to authenticated superusers.
 * Non-superusers are redirected to the home page.
 */
export const adminGuard: CanActivateFn = () => {
  const authService = inject(AuthService);
  const router = inject(Router);

  return authService.initialized$.pipe(
    map(() => {
      if (authService.user()?.superuser === true) {
        return true;
      }
      router.navigate(['/']);
      return false;
    })
  );
};
