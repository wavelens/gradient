/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { HttpInterceptorFn, HttpErrorResponse } from '@angular/common/http';
import { inject } from '@angular/core';
import { Router } from '@angular/router';
import { Observable, catchError, retry, throwError, timer } from 'rxjs';

const THROTTLE_RETRIES = 3;
const MIN_THROTTLE_WAIT_MS = 250;

function waitOutThrottle(error: unknown): Observable<number> {
  if (!(error instanceof HttpErrorResponse) || error.status !== 429) return throwError(() => error);
  const seconds = Number(error.headers.get('retry-after'));
  return timer(Math.max(MIN_THROTTLE_WAIT_MS, Number.isFinite(seconds) ? seconds * 1000 : 0));
}

/**
 * HTTP interceptor that handles global error responses.
 *
 * - 401: clears auth tokens, redirects to login
 * - 502/503/504/0: server unavailable, render the matching error page in
 *   place WITHOUT changing the URL - so F5 reloads the user's original
 *   route and re-tries the failing request rather than reloading the
 *   error page itself.
 * - Other errors: re-thrown so individual components can handle them
 */
export const errorInterceptor: HttpInterceptorFn = (req, next) => {
  const router = inject(Router);

  const showErrorPage = (status: number) => {
    router.navigate([`/error/${status}`], {
      queryParams: { from: router.url },
      skipLocationChange: true,
    });
  };

  return next(req).pipe(
    retry({ count: THROTTLE_RETRIES, delay: waitOutThrottle }),
    catchError((error: HttpErrorResponse) => {
      switch (error.status) {
        case 401: {
          localStorage.removeItem('jwt_token');
          sessionStorage.removeItem('jwt_token');
          const pending = router.currentNavigation()?.finalUrl;
          const next = pending ? router.serializeUrl(pending) : router.url;
          const carry = next && next !== '/' && !next.startsWith('/account/');
          router.navigate(['/account/login'], carry ? { queryParams: { next } } : {});
          break;
        }

        case 0:
          // Network error or server completely unreachable - treat as 503
          showErrorPage(503);
          break;

        case 429:
        case 502:
        case 503:
        case 504:
          showErrorPage(error.status);
          break;

        case 403:
          console.error('Access denied:', error.message);
          break;

        default:
          if (error.status >= 500) {
            console.error('Server error:', error.message);
          }
          break;
      }

      return throwError(() => error);
    })
  );
};
