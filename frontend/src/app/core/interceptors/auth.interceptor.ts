/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { HttpInterceptorFn } from '@angular/common/http';

/**
 * HTTP interceptor that adds the Bearer token to all outgoing requests
 */
export const authInterceptor: HttpInterceptorFn = (req, next) => {
  // Get token from localStorage or sessionStorage
  const token =
    localStorage.getItem('jwt_token') || sessionStorage.getItem('jwt_token');

  // If token exists and this is an API request, add Authorization header
  if (token && req.url.includes('/api/v1')) {
    req = req.clone({
      setHeaders: {
        Authorization: `Bearer ${token}`,
      },
    });
  }

  return next(req);
};
