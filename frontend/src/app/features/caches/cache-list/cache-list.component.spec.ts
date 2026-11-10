/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { Router, provideRouter } from '@angular/router';
import { By } from '@angular/platform-browser';
import { CreateCacheDialogComponent } from '@shared/forms';
import { Observable, of, throwError } from 'rxjs';
import { CacheListComponent } from './cache-list.component';
import { CachesService } from '@core/services/caches.service';
import { ConfigService } from '@core/services/config.service';
import { AuthService } from '@core/services/auth.service';
import { ApiError } from '@core/services/api.service';
import { UserService } from '@core/services/user.service';
import { Cache, UserAccessSummary } from '@core/models';

const OWN = { id: 'c1', name: 'main', display_name: 'Main', active: true, public: false, priority: 10 } as Cache;
const PUBLIC = { id: 'c2', name: 'shared', display_name: 'Shared', active: true, public: true, priority: 20 } as Cache;

function setup(opts: {
  authenticated: boolean;
  getCaches?: () => Observable<{ items: Cache[] }>;
  getPublicCaches?: () => Observable<Cache[]>;
  accessSummary?: UserAccessSummary | null;
}) {
  const getCaches = vi.fn(opts.getCaches ?? (() => of({ items: [OWN] })));
  const getPublicCaches = vi.fn(opts.getPublicCaches ?? (() => of([PUBLIC])));
  TestBed.configureTestingModule({
    imports: [CacheListComponent],
    providers: [
      provideRouter([]),
      { provide: UserService, useValue: { getProjectsCachesAndTeamsCurrentUserCanReach: () => of(opts.accessSummary ?? null), getInvites: () => of([]) } },
      { provide: CachesService, useValue: { getCaches, getPublicCaches } },
      { provide: ConfigService, useValue: { publicListing: true, createCache: 'everyone', canCreate: () => true } },
      { provide: AuthService, useValue: { isAuthenticated: () => opts.authenticated, user: () => null } },
    ],
  });
  const fixture = TestBed.createComponent(CacheListComponent);
  fixture.detectChanges();
  const element = fixture.nativeElement as HTMLElement;
  return { fixture, element, getCaches };
}

describe('CacheListComponent', () => {
  it('lists own and public caches', () => {
    const { element } = setup({ authenticated: true });
    expect(element.textContent).toContain('Main');
    expect(element.textContent).toContain('Shared');
  });

  it('shows a failed load with Try Again instead of the empty state', () => {
    const getCaches = vi.fn()
      .mockReturnValueOnce(throwError(() => new ApiError('database is down', 500)))
      .mockReturnValue(of({ items: [OWN] }));
    const { fixture, element } = setup({ authenticated: true, getCaches });
    expect(element.textContent).toContain('Failed to load caches');
    expect(element.textContent).not.toContain('No caches yet');
    element.querySelector<HTMLButtonElement>('.gr-load-state__error button')?.click();
    fixture.detectChanges();
    expect(getCaches).toHaveBeenCalledTimes(2);
    expect(element.textContent).toContain('Main');
  });

  it('refreshes the public caches together with the own list', () => {
    vi.useFakeTimers();
    try {
      const getPublicCaches = vi.fn().mockReturnValueOnce(of([])).mockReturnValue(of([PUBLIC]));
      const { fixture, element } = setup({ authenticated: true, getPublicCaches });
      expect(element.textContent).not.toContain('Shared');
      vi.advanceTimersByTime(15_000);
      fixture.detectChanges();
      expect(getPublicCaches).toHaveBeenCalledTimes(2);
      expect(element.textContent).toContain('Shared');
    } finally {
      vi.useRealTimers();
    }
  });

  it('shows a failed public list as an error', () => {
    const { element } = setup({ authenticated: false, getPublicCaches: () => throwError(() => new ApiError('down', 500)) });
    expect(element.textContent).toContain('Failed to load public caches');
    expect(element.textContent).not.toContain('No caches yet');
  });

  it('does not claim there are no caches above a list of public caches', () => {
    const { element } = setup({ authenticated: false });
    expect(element.textContent).toContain('Shared');
    expect(element.textContent).not.toContain('No caches yet');
    expect(element.textContent).toContain('Public caches on this server.');
    expect(element.textContent).not.toContain('Manage your build caches');
  });

  it('shows the empty state for a signed-in user without caches, also above public caches', () => {
    const { element } = setup({ authenticated: true, getCaches: () => of({ items: [] }) });
    expect(element.textContent).toContain('No caches yet');
    expect(element.textContent).toContain('Shared');
    expect(element.querySelectorAll('app-create-cache-button')).toHaveLength(1);
  });

  it('offers Create in the header once the list has caches', () => {
    const { element } = setup({ authenticated: true });
    expect(element.querySelectorAll('app-create-cache-button')).toHaveLength(1);
    expect(element.textContent).toContain('Manage your build caches');
  });

  it('opens the created cache', () => {
    const { fixture } = setup({ authenticated: true });
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigateByUrl').mockResolvedValue(true);
    fixture.debugElement.query(By.directive(CreateCacheDialogComponent)).componentInstance.created.emit('team-cache');
    expect(String(navigate.mock.calls[0][0])).toBe('/caches/team-cache');
  });

  it('shows the effective role and owner team from the access overview', () => {
    const accessSummary: UserAccessSummary = {
      superuser: false,
      projects: [],
      caches: [{ name: 'main', display_name: 'Main', role: 'Read', owner_team: { name: 'infra', display_name: 'Infra' }, reasons: [] }],
      teams: [],
    };
    const { element } = setup({ authenticated: true, getPublicCaches: () => of([]), accessSummary });
    expect(element.querySelector('gr-nav-card')?.textContent).toContain('Main · Infra');
    expect(Array.from(element.querySelectorAll('gr-badge')).map((badge) => badge.textContent?.trim())).toEqual(['Read']);
  });
});
