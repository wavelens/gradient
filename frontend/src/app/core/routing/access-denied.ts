/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { inject } from '@angular/core';
import { RedirectCommand, Router } from '@angular/router';
import { EMPTY, Observable, OperatorFunction, catchError, map, of, switchMap } from 'rxjs';
import { ApiError } from '@core/services/api.service';
import { AuthService } from '@core/services/auth.service';

const entityLookupNotFoundOrForbiddenStatusCodes = new Set([400, 403, 404]);

export type DeniedKind = 'project' | 'task' | 'cache' | 'team' | 'evaluation' | 'build' | 'execJob' | 'job' | 'clusterJob' | 'worker';

export interface DeniedTarget {
  kind: DeniedKind;
  name: string;
  back: string;
}

/// The app can show the shared Not Found page in place. The address bar can keep the link that the user followed.
export function deniedPage(router: Router, target: DeniedTarget, from: string): RedirectCommand {
  const tree = router.createUrlTree(['/error/404'], { queryParams: { ...target, from } });
  return new RedirectCommand(tree, { browserUrl: from });
}

export function adminOnlyPage(router: Router, from: string): RedirectCommand {
  return new RedirectCommand(router.createUrlTree(['/error/403'], { queryParams: { from } }), { browserUrl: from });
}

export function denyOnRefusal<T>(target: DeniedTarget, from: string): OperatorFunction<T, T | RedirectCommand> {
  const router = inject(Router);
  const auth = inject(AuthService);
  return catchError((error: unknown): Observable<RedirectCommand> => {
    if (error instanceof ApiError && entityLookupNotFoundOrForbiddenStatusCodes.has(error.status)) {
      return auth.initialized$.pipe(
        switchMap(() => (auth.isAuthenticated() ? auth.stillSignedIn() : of(false))),
        map((signedIn) => {
          if (signedIn) return deniedPage(router, target, from);
          if (auth.isAuthenticated()) return new RedirectCommand(auth.forgetRejectedSessionAndGetSignInPageUrl(from));
          return new RedirectCommand(auth.getSignInPageUrl(from));
        }),
      );
    }
    if (error instanceof ApiError && (error.unreachable || error.status === 401)) return EMPTY;
    const failed = router.createUrlTree(['/error/500'], { queryParams: { from } });
    return of(new RedirectCommand(failed, { browserUrl: from }));
  });
}
