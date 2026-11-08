/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { ActivatedRouteSnapshot, convertToParamMap } from '@angular/router';
import { Observable, firstValueFrom, of } from 'rxjs';
import { CachesService } from '@core/services/caches.service';
import { cacheAccessResolver, CacheAccessData, shouldRefetchCacheRecordOnCacheNameOrCacheRecordPageChange } from './cache-access.resolver';
import { CacheResponse } from '@core/models';

function snap(params: Record<string, string>): ActivatedRouteSnapshot {
  return { paramMap: convertToParamMap(params) } as ActivatedRouteSnapshot;
}

function runResolver(
  route: ActivatedRouteSnapshot,
): Promise<CacheAccessData> {
  const result = TestBed.runInInjectionContext(() =>
    cacheAccessResolver(route, {} as never),
  ) as Observable<CacheAccessData>;
  return firstValueFrom(result);
}

describe('cacheAccessResolver', () => {
  let getCache: ReturnType<typeof vi.fn>;

  const baseCache: CacheResponse = {
    id: 'c1',
    name: 'demo',
    display_name: 'Demo',
    description: '',
    active: true,
    priority: 10,
    local_priority: null,
    max_storage_gb: 0,
    pull_through: true,
    public_key: 'cache:key',
    public: false,
    created_by: 'u1',
    created_at: '2026-01-01T00:00:00',
    managed: false,
    can_edit: true,
    permissions: ['manageCacheSettings'],
  };

  beforeEach(() => {
    getCache = vi.fn(() => of(baseCache));
    TestBed.configureTestingModule({
      providers: [{ provide: CachesService, useValue: { getCache } }],
    });
  });

  it('fetches the cache by route param and exposes access state', async () => {
    const data = await runResolver(snap({ cache: 'demo' }));
    expect(getCache).toHaveBeenCalledWith('demo');
    expect(data.cache).toBe(baseCache);
    expect(data.access).toEqual({ managed: false, canEdit: true, canTrigger: true });
  });

  it('propagates managed and can_edit into access', async () => {
    getCache.mockReturnValue(
      of({ ...baseCache, managed: true, can_edit: false }),
    );
    const data = await runResolver(snap({ cache: 'demo' }));
    expect(data.access).toEqual({ managed: true, canEdit: false, canTrigger: false });
  });
});

describe('shouldRefetchCacheRecordOnCacheNameOrCacheRecordPageChange', () => {
  const at = (cache: string, path: string) =>
    ({ paramMap: convertToParamMap({ cache }), firstChild: { routeConfig: { path } } }) as unknown as ActivatedRouteSnapshot;
  const refetch = shouldRefetchCacheRecordOnCacheNameOrCacheRecordPageChange as (from: ActivatedRouteSnapshot, to: ActivatedRouteSnapshot) => boolean;

  it('fetches the cache again on entering the pages that show the record', () => {
    const nars = at('demo', 'nars');
    const overview = at('demo', '');
    expect(refetch(nars, overview)).toBe(true);
    expect(refetch(overview, at('demo', 'settings'))).toBe(true);
  });

  it('keeps the resolved cache for other tabs and query changes on the same page', () => {
    const overview = at('demo', '');
    expect(refetch(overview, at('demo', 'nars'))).toBe(false);
    expect(refetch(overview, { ...overview } as ActivatedRouteSnapshot)).toBe(false);
  });
});
