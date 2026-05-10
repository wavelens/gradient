/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { inject } from '@angular/core';
import { ResolveFn } from '@angular/router';
import { map } from 'rxjs';
import { CachesService } from '@core/services/caches.service';
import { Cache } from '@core/models/cache.model';
import { AccessState, accessFromEntity } from '@core/models/access.model';

export interface CacheAccessData {
  cache: Cache;
  access: AccessState;
}

export const cacheAccessResolver: ResolveFn<CacheAccessData> = (route) => {
  const caches = inject(CachesService);
  const cache = route.paramMap.get('cache') ?? '';
  return caches.getCache(cache).pipe(
    map((c) => ({ cache: c, access: accessFromEntity(c) })),
  );
};
