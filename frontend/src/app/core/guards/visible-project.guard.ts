/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { inject } from '@angular/core';
import { CanActivateFn, Router } from '@angular/router';
import { of } from 'rxjs';
import { catchError, map, switchMap } from 'rxjs/operators';
import { ApiError } from '@core/services/api.service';
import { AuthService } from '@core/services/auth.service';
import { ProjectsService } from '@core/services/projects.service';

// Private and missing projects are the same 404 to anonymous visitors: send both to login, so project names stay secret.
export const visibleProjectGuard: CanActivateFn = (route, state) => {
  const authService = inject(AuthService);
  const projects = inject(ProjectsService);
  const login = inject(Router).createUrlTree(['/account/login'], { queryParams: { next: state.url } });

  return authService.initialized$.pipe(
    switchMap(() => authService.resolveSession()),
    switchMap((authenticated) => {
      if (authenticated) {
        return of(true);
      }
      if (authService.serverUnreachable()) {
        return of(false);
      }
      return projects.getProject(route.paramMap.get('project') ?? '').pipe(
        map(() => true),
        catchError((error: unknown) => of(error instanceof ApiError && error.status === 404 ? login : true)),
      );
    }),
  );
};
