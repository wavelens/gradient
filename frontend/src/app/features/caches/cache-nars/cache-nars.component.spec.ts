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
import { CachesService } from '@core/services/caches.service';
import { NarSummary, NarStats, Paginated } from '@core/models';
import { ApiError } from '@core/services/api.service';

const NAR = {
  hash: 'abc123',
  store_path: '/nix/store/abc123-hello',
  package: 'hello',
  nar_size: 1024,
  file_size: 512,
  created_at: '2026-10-05T10:00:00',
  last_fetched_at: null,
};
const PAGE: Paginated<NarSummary[]> = { items: [NAR], total: 1, page: 1, per_page: 50 };

function render(nars: () => Observable<Paginated<NarSummary[]>>, stats: () => Observable<NarStats> = () => NEVER) {
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

  it('replaces stale rows with the error when a reload fails, and loads again on Try Again', () => {
    let calls = 0;
    const { fixture, query, root } = render(() => (++calls === 2 ? throwError(() => ({ status: 500, message: 'database is down' })) : of(PAGE)));
    query.next(convertToParamMap({ page: '2' }));
    fixture.detectChanges();
    expect(root.textContent).toContain('Failed to load NARs');
    expect(root.textContent).toContain('database is down');
    expect(root.querySelector('tbody')).toBeNull();
    expect(root.textContent).not.toContain('No NARs found');
    root.querySelector<HTMLButtonElement>('.gr-load-state__error button')?.click();
    fixture.detectChanges();
    expect(root.querySelector('tbody')?.textContent).toContain('abc123');
  });

  it('drops the stat cards when the stats fail', () => {
    const { root } = render(() => of(PAGE), () => throwError(() => ({ status: 500 })));
    expect(root.querySelector('gr-stat-card')).toBeNull();
  });
});

describe('CacheNarsComponent delete', () => {
  it('keeps a failed delete inside the open dialog and clears it on the next attempt', () => {
    const { fixture } = render(() => of(PAGE));
    const service = TestBed.inject(CachesService) as unknown as { deleteCacheNar: () => unknown };
    service.deleteCacheNar = () => throwError(() => new ApiError('NAR is pinned by a running build', 409));
    const cmp = fixture.componentInstance;
    cmp.askDelete(NAR);
    fixture.detectChanges();
    cmp.confirmDelete();
    fixture.detectChanges();
    expect(document.querySelector('.gr-dialog gr-message-banner')?.textContent).toContain('NAR is pinned by a running build');
    expect(cmp.pendingDelete()).toEqual(NAR);

    cmp.cancelDelete();
    cmp.askDelete(NAR);
    fixture.detectChanges();
    expect(document.querySelector('.gr-dialog gr-message-banner')).toBeNull();
  });

  it('keeps the delete dialog open with its close button while the delete runs', () => {
    const { fixture } = render(() => of(PAGE));
    const service = TestBed.inject(CachesService) as unknown as { deleteCacheNar: () => unknown };
    service.deleteCacheNar = vi.fn(() => NEVER);
    const cmp = fixture.componentInstance;
    cmp.askDelete(NAR);
    fixture.detectChanges();
    cmp.confirmDelete();
    fixture.detectChanges();
    const dialog = document.querySelector('.gr-dialog') as HTMLElement;
    dialog.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', keyCode: 27, bubbles: true }));
    (document.querySelector('.gr-dialog-backdrop') as HTMLElement).click();
    fixture.detectChanges();
    expect(cmp.pendingDelete()).toEqual(NAR);
    expect(dialog.getAttribute('aria-busy')).toBe('true');
    expect(dialog.querySelector<HTMLButtonElement>('.gr-dialog__close')?.disabled).toBe(true);
  });
});
