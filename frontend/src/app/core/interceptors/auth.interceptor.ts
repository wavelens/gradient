/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { HttpInterceptorFn } from '@angular/common/http';

/**
 * HTTP interceptor that enables cookie-based auth for all API requests.
 * The JWT is stored in an httpOnly cookie; withCredentials ensures the
 * browser sends it automatically on every request.
 */
export const authInterceptor: HttpInterceptorFn = (req, next) => {
  if (req.url.includes('/api/v1')) {
    req = req.clone({ withCredentials: true });
  }

  return next(req);
};
