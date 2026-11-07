/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { inject } from '@angular/core';
import { CanActivateFn } from '@angular/router';
import { of } from 'rxjs';
import { map, switchMap } from 'rxjs/operators';
import { AuthService } from '@core/services/auth.service';
import { ProjectsService } from '@core/services/projects.service';
import { denyOnRefusal } from '@core/routing/access-denied';
import { links } from '@shared/routing';

export const visibleProjectGuard: CanActivateFn = (route, state) => {
  const authService = inject(AuthService);
  const projects = inject(ProjectsService);
  const name = route.paramMap.get('project') ?? '';
  const deny = denyOnRefusal<true>({ kind: 'project', name, back: String(links.projects()) }, state.url);

  return authService.initialized$.pipe(
    switchMap(() => authService.resolveSession()),
    switchMap(() => {
      if (authService.serverUnreachable()) return of(false);
      return projects.getProject(name).pipe(map(() => true as const), deny);
    }),
  );
};
