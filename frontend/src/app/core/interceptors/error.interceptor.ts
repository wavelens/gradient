/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { HttpErrorResponse, HttpInterceptorFn } from '@angular/common/http';
import { Injector, inject } from '@angular/core';
import { PRIMARY_OUTLET, Router } from '@angular/router';
import { MessageService } from '@gradient/ui/ui';
import { Observable, catchError, retry, throwError, timer } from 'rxjs';
import { AuthService } from '@core/services/auth.service';
import { BACKGROUND, SESSION_PROBE } from './request-context';
import { PageRenderTracker } from './page-render-tracker.service';

const THROTTLE_RETRIES = 3;
const MIN_THROTTLE_WAIT_MS = 250;
export const MAX_THROTTLE_WAIT_MS = 30_000;
const NOTICE_THRESHOLD_MS = 1_000;
const READ_METHODS = new Set(['GET', 'HEAD', 'OPTIONS']);
const ERROR_PAGES = new Set([429, 502, 503, 504]);

export function throttleWaitMs(error: HttpErrorResponse): number {
  const header = error.headers?.get('retry-after') ?? null;
  const seconds = Number(header);
  const named = header === null ? 0 : Number.isFinite(seconds) ? seconds * 1000 : Date.parse(header) - Date.now();
  return Math.max(MIN_THROTTLE_WAIT_MS, Number.isFinite(named) ? named : 0);
}

function pageUrl(router: Router): string {
  const tree = router.currentNavigation()?.finalUrl ?? router.parseUrl(router.url);
  if (tree.root.children[PRIMARY_OUTLET]?.segments[0]?.path !== 'error') return router.serializeUrl(tree);
  const from = tree.queryParamMap.get('from');
  return from?.startsWith('/') && !from.startsWith('/error/') ? from : '/';
}

/// Only reads that render the page can replace the page with an error page or show the throttle notice. The interceptor can retry reads after a rate limit. Writes have no retry. The server can apply a resent write twice. The interceptor can rethrow later reads (polling, retry, search in the command palette) to the inline error state on the page.
export const errorInterceptor: HttpInterceptorFn = (req, next) => {
  const router = inject(Router);
  const injector = inject(Injector);
  const read = READ_METHODS.has(req.method);
  const pageRead = read && !req.context.get(BACKGROUND) && inject(PageRenderTracker).isPageStillRendering();
  const probe = req.context.get(SESSION_PROBE);
  const notify = (detail: string, life?: number) =>
    injector.get(MessageService).add({ severity: 'warn', summary: 'Server Busy', detail, life, key: 'throttle' });

  const throttled = (error: unknown): Observable<number> => {
    if (!(error instanceof HttpErrorResponse) || error.status !== 429) return throwError(() => error);
    const wait = throttleWaitMs(error);
    if (wait > MAX_THROTTLE_WAIT_MS) return throwError(() => error);
    if (pageRead && wait >= NOTICE_THRESHOLD_MS) notify(`Retry in ${Math.ceil(wait / 1000)} s.`, wait);
    return timer(wait);
  };

  const handle = (error: HttpErrorResponse): void => {
    const auth = injector.get(AuthService);
    if (!probe && error.status === 401) {
      auth.sessionLost(pageUrl(router));
      return;
    }
    if (!probe && error.status === 403 && auth.isAuthenticated()) {
      const returnUrl = pageUrl(router);
      auth.stillSignedIn().subscribe((signedIn) => signedIn || auth.sessionLost(returnUrl));
      return;
    }
    if (error.status === 429 && !read) {
      notify(`Try again in ${Math.ceil(throttleWaitMs(error) / 1000)} seconds.`);
      return;
    }
    const status = error.status === 0 ? 503 : error.status;
    if (pageRead && ERROR_PAGES.has(status)) {
      router.navigate([`/error/${status}`], { queryParams: { from: pageUrl(router) }, skipLocationChange: true });
    }
  };

  return next(req).pipe(
    retry({ count: read ? THROTTLE_RETRIES : 0, delay: throttled }),
    catchError((error: unknown) => {
      if (error instanceof HttpErrorResponse) handle(error);
      return throwError(() => error);
    }),
  );
};
