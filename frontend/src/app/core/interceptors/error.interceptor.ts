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
 * HTTP interceptor that handles global error responses
 */
export const errorInterceptor: HttpInterceptorFn = (req, next) => {
  const router = inject(Router);

  return next(req).pipe(
    catchError((error: HttpErrorResponse) => {
      // Handle different HTTP error status codes
      if (error.status === 401) {
        // Unauthorized - redirect to login
        localStorage.removeItem('jwt_token');
        sessionStorage.removeItem('jwt_token');
        router.navigate(['/account/login']);
      } else if (error.status === 403) {
        // Forbidden - show error message
        console.error('Access denied:', error.message);
      } else if (error.status >= 500) {
        // Server error
        console.error('Server error:', error.message);
      }

      return throwError(() => error);
    })
  );
};
