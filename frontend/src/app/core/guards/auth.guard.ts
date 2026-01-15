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
 * Route guard that redirects unauthenticated users to the login page.
 * Waits for the initial auth check to complete before deciding.
 */
export const authGuard: CanActivateFn = () => {
  const authService = inject(AuthService);
  const router = inject(Router);

  return authService.initialized$.pipe(
    map(() => {
      if (authService.isAuthenticated()) {
        return true;
      }
      router.navigate(['/account/login']);
      return false;
    })
  );
};
