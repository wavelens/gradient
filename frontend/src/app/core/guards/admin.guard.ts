/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { inject } from '@angular/core';
import { Router, CanActivateFn } from '@angular/router';
import { map } from 'rxjs/operators';
import { AuthService } from '@core/services/auth.service';
import { adminOnlyPage } from '@core/routing/access-denied';

export const adminGuard: CanActivateFn = (_route, state) => {
  const authService = inject(AuthService);
  const router = inject(Router);

  return authService.initialized$.pipe(
    map(() => authService.user()?.superuser === true || adminOnlyPage(router, state.url)),
  );
};
