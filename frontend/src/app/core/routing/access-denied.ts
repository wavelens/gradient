/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { inject } from '@angular/core';
import { RedirectCommand, Router } from '@angular/router';
import { EMPTY, Observable, OperatorFunction, catchError, of } from 'rxjs';
import { ApiError } from '@core/services/api.service';

export type DeniedKind = 'project' | 'task' | 'cache' | 'team' | 'evaluation' | 'build' | 'execJob' | 'job' | 'clusterJob' | 'worker';

export interface DeniedTarget {
  kind: DeniedKind;
  name: string;
  back: string;
}

/// The app can show the shared Not Found page in place. The address bar can keep the link that the user followed.
export function deniedPage(router: Router, target: DeniedTarget, from: string): RedirectCommand {
  const tree = router.createUrlTree(['/error/404'], { queryParams: { ...target, from } });
  return new RedirectCommand(tree, { skipLocationChange: true });
}

export function adminOnlyPage(router: Router, from: string): RedirectCommand {
  return new RedirectCommand(router.createUrlTree(['/error/403'], { queryParams: { from } }), { skipLocationChange: true });
}

export function denyOnRefusal<T>(target: DeniedTarget, from: string): OperatorFunction<T, T | RedirectCommand> {
  const router = inject(Router);
  return catchError((error: unknown): Observable<RedirectCommand> => {
    if (error instanceof ApiError && (error.status === 403 || error.status === 404)) return of(deniedPage(router, target, from));
    if (error instanceof ApiError && error.unreachable) return EMPTY;
    const failed = router.createUrlTree(['/error/500'], { queryParams: { from } });
    return of(new RedirectCommand(failed, { skipLocationChange: true }));
  });
}
