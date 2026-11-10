/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Signal, computed, inject } from '@angular/core';
import { toObservable, toSignal } from '@angular/core/rxjs-interop';
import { catchError, map, of, switchMap } from 'rxjs';
import { CachesService } from '@core/services/caches.service';
import { injectCacheAccessData } from '@core/resolvers/inject-access';

export function injectPendingSubscriptionRequestCount(): Signal<number | null> {
  const caches = inject(CachesService);
  const cacheData = injectCacheAccessData();
  const cacheNameIfViewerCanManageSubscriptions = computed(() => {
    const cache = cacheData()?.cache;
    return cache?.permissions?.includes('manageCacheSubscriptions') ? cache.name : null;
  });
  return toSignal(
    toObservable(cacheNameIfViewerCanManageSubscriptions).pipe(
      switchMap((name) =>
        name === null
          ? of(null)
          : caches.getSubscriptionRequests(name).pipe(
              map((requests) => requests.length),
              catchError(() => of(null)),
            ),
      ),
    ),
    { initialValue: null },
  );
}
