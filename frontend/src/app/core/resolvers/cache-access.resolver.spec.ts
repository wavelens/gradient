/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { ActivatedRouteSnapshot, convertToParamMap } from '@angular/router';
import { Observable, firstValueFrom, of } from 'rxjs';
import { CachesService } from '@core/services/caches.service';
import { cacheAccessResolver, CacheAccessData } from './cache-access.resolver';
import { Cache } from '@core/models/cache.model';

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

  const baseCache: Cache = {
    id: 'c1',
    name: 'demo',
    display_name: 'Demo',
    description: '',
    active: true,
    priority: 10,
    public: false,
    managed: false,
    can_edit: true,
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
    expect(data.access).toEqual({ managed: false, canEdit: true });
  });

  it('propagates managed and can_edit into access', async () => {
    getCache.mockReturnValue(
      of({ ...baseCache, managed: true, can_edit: false }),
    );
    const data = await runResolver(snap({ cache: 'demo' }));
    expect(data.access).toEqual({ managed: true, canEdit: false });
  });
});
