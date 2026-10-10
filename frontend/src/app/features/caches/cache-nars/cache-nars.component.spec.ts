/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { BehaviorSubject, NEVER, Observable, of, throwError } from 'rxjs';
import { CacheNarsComponent } from './cache-nars.component';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { CachesService, NarListResponse, NarStats } from '@core/services/caches.service';

const NAR = {
  hash: 'abc123',
  store_path: '/nix/store/abc123-hello',
  package: 'hello',
  nar_size: 1024,
  file_size: 512,
  created_at: '2026-10-05T10:00:00',
  last_fetched_at: null,
};
const PAGE: NarListResponse = { items: [NAR], total: 1, page: 1, per_page: 50 };

function render(nars: () => Observable<NarListResponse>, stats: () => Observable<NarStats> = () => NEVER) {
  const query = new BehaviorSubject(convertToParamMap({}));
  const getCache = vi.fn(() => NEVER);
  TestBed.configureTestingModule({
    imports: [CacheNarsComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      {
        provide: CachesService,
        useValue: { getCache, getCacheNars: vi.fn(nars), getCacheNarStats: stats },
      },
      {
        provide: ActivatedRoute,
        useValue: {
          snapshot: { paramMap: convertToParamMap({ cache: 'main' }) },
          queryParamMap: query,
          parent: { data: of({}) },
        },
      },
    ],
  });
  TestBed.inject(BreadcrumbsService).rememberCache('main', 'Main');
  const fixture = TestBed.createComponent(CacheNarsComponent);
  fixture.detectChanges();
  return { fixture, query, getCache, root: fixture.nativeElement as HTMLElement };
}

describe('CacheNarsComponent loading', () => {
  it('names the cache in the breadcrumb at once from the name the resolver remembered', () => {
    const { root, getCache } = render(() => NEVER);
    const crumbs = Array.from(root.querySelectorAll('.breadcrumb-link, .breadcrumb-current'));
    expect(crumbs.map((crumb) => crumb.textContent?.trim())).toEqual(['Caches', 'Main', 'NARs']);
    expect(getCache).not.toHaveBeenCalled();
  });

  it('lays out the stat cards and the table with placeholders until the data arrives', () => {
    const { root } = render(() => NEVER);
    expect(root.querySelector('gr-loading-spinner')).toBeNull();
    expect(root.querySelectorAll('.stat-value gr-skeleton').length).toBe(5);
    const table = root.querySelector('gr-table')!;
    expect(table.getAttribute('aria-busy')).toBe('true');
    expect(table.querySelectorAll('th').length).toBeGreaterThan(0);
    expect(table.querySelector('tbody gr-skeleton')).not.toBeNull();
    expect(root.querySelector('.pagination')).toBeNull();
  });

  it('keeps the current rows while the next page loads', () => {
    let calls = 0;
    const { fixture, query, root } = render(() => (++calls === 1 ? of(PAGE) : NEVER));
    query.next(convertToParamMap({ page: '2' }));
    fixture.detectChanges();
    expect(root.querySelector('tbody')?.textContent).toContain('abc123');
    expect(root.querySelector('tbody gr-skeleton')).toBeNull();
    expect(root.querySelector('gr-table')?.getAttribute('aria-busy')).toBe('true');
  });

  it('drops the stat cards when the stats fail', () => {
    const { root } = render(() => of(PAGE), () => throwError(() => ({ status: 500 })));
    expect(root.querySelector('gr-stat-card')).toBeNull();
  });
});
