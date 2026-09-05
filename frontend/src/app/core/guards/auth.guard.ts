/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { inject } from '@angular/core';
import { Router, CanActivateFn } from '@angular/router';
import { switchMap, map } from 'rxjs/operators';
import { AuthService } from '@core/services/auth.service';

/**
 * Route guard that redirects unauthenticated users to the login page.
 * Waits for the initial auth check to complete before deciding.
 *
 * A server that never answered is not an answer: the session is re-probed, and
 * while it stays unreachable the navigation is only cancelled, leaving the
 * error page the interceptor put up instead of claiming the user is logged out.
 */
export const authGuard: CanActivateFn = (_route, state) => {
  const authService = inject(AuthService);
  const router = inject(Router);

  return authService.initialized$.pipe(
    switchMap(() => authService.resolveSession()),
    map((authenticated) => {
      if (authenticated) {
        return true;
      }
      if (authService.serverUnreachable()) {
        return false;
      }
      router.navigate(['/account/login'], { queryParams: { next: state.url } });
      return false;
    })
  );
};
