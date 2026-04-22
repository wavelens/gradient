/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { HttpInterceptorFn, HttpErrorResponse } from '@angular/common/http';
import { inject } from '@angular/core';
import { Router } from '@angular/router';
import { catchError, throwError } from 'rxjs';

/**
 * HTTP interceptor that handles global error responses.
 *
 * - 401: clears auth tokens, redirects to login
 * - 502/503/504/0: server unavailable, redirects to the matching error page
 * - Other errors: re-thrown so individual components can handle them
 */
export const errorInterceptor: HttpInterceptorFn = (req, next) => {
  const router = inject(Router);

  return next(req).pipe(
    catchError((error: HttpErrorResponse) => {
      switch (error.status) {
        case 401:
          localStorage.removeItem('jwt_token');
          sessionStorage.removeItem('jwt_token');
          router.navigate(['/account/login']);
          break;

        case 0:
          // Network error or server completely unreachable — treat as 503
          router.navigate(['/error/503'], { queryParams: { from: router.url } });
          break;

        case 502:
        case 503:
        case 504:
          router.navigate([`/error/${error.status}`], { queryParams: { from: router.url } });
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
