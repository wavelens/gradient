/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { NEVER, of, throwError } from 'rxjs';
import { ApiError } from '@core/services/api.service';
import { CacheDetailComponent } from './cache-detail.component';
import { CachesService } from '@core/services/caches.service';
import { AuthService } from '@core/services/auth.service';
import { StarsService } from '@core/services/stars.service';

const OWN_KEY = 'public.gradient.ci-main:74DUi4Ye579gUqzH4ziL9IyiJBlDpMRn9MBN8oNan9M=';
const UPSTREAM_KEY = 'cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY=';

describe('CacheDetailComponent cache usage instructions', () => {
  function setup(cache: object | null = { name: 'main', public_key: OWN_KEY, public: true }) {
    const caches = {
      getCacheStats: vi.fn().mockReturnValue(of(null)),
      getSubscribers: () => NEVER,
    };
    TestBed.configureTestingModule({
      imports: [CacheDetailComponent],
      providers: [
        provideRouter([]),
        provideHttpClient(),
        provideHttpClientTesting(),
        { provide: CachesService, useValue: caches },
        {
          provide: ActivatedRoute,
          useValue: {
            snapshot: { paramMap: convertToParamMap({ cache: 'main' }), queryParamMap: convertToParamMap({}) },
            parent: { data: of({ cacheAccess: cache ? { cache } : undefined }) },
          },
        },
      ],
    });
    // ngOnInit directly rather than detectChanges: rendering pulls in the
    // metric charts, which is not what these assertions are about.
    const component = TestBed.createComponent(CacheDetailComponent).componentInstance;
    component.ngOnInit();
    return component;
  }

  // The server re-signs everything it serves, proxied paths included, so this
  // one key is all a client needs. Listing the upstream caches' keys made every user
  // of the cache configure a key for every cache it happens to proxy.
  it('trusts only the cache own signing key', () => {
    const component = setup();

    expect(component.trustedPublicKeys()).toEqual([OWN_KEY]);
    expect(component.nixConfSnippet()).toContain(OWN_KEY);
    expect(component.nixConfSnippet()).not.toContain(UPSTREAM_KEY);
  });

  // A cache whose key has not been generated yet must not render a snippet that
  // looks copy-pasteable but silently trusts nothing.
  it('says so when no key is available', () => {
    const component = setup(null);

    expect(component.trustedPublicKeys()).toEqual([]);
    expect(component.nixConfSnippet()).toContain('<unavailable>');
  });
});

describe('CacheDetailComponent loading', () => {
  it('shows the resolved cache at once and holds the stats with placeholders', () => {
    TestBed.configureTestingModule({
      imports: [CacheDetailComponent],
      providers: [
        provideRouter([]),
        provideHttpClient(),
        provideHttpClientTesting(),
        { provide: CachesService, useValue: { getCache: () => NEVER, getCacheStats: () => NEVER, getSubscribers: () => NEVER } },
        {
          provide: ActivatedRoute,
          useValue: {
            snapshot: { paramMap: convertToParamMap({ cache: 'main' }), queryParamMap: convertToParamMap({}) },
            parent: { data: of({ cacheAccess: { cache: { name: 'main', display_name: 'Main', active: true, public: true } } }) },
          },
        },
        { provide: StarsService, useValue: { starred: () => of(false), set: () => of(true) } },
      ],
    });
    const fixture = TestBed.createComponent(CacheDetailComponent);
    fixture.detectChanges();
    const root = fixture.nativeElement as HTMLElement;

    expect(root.querySelector('gr-loading-spinner')).toBeNull();
    expect(root.querySelector('h1')?.textContent).toContain('Main');
    expect(root.querySelectorAll('.stat-value gr-skeleton').length).toBe(5);
    expect(root.querySelectorAll('gr-metric-chart gr-skeleton').length).toBe(2);
  });

  it('shows failed stats as an error with Try Again, not as zeros', () => {
    const getCacheStats = vi.fn().mockReturnValueOnce(throwError(() => new ApiError('database is down', 500))).mockReturnValue(NEVER);
    TestBed.configureTestingModule({
      imports: [CacheDetailComponent],
      providers: [
        provideRouter([]),
        provideHttpClient(),
        provideHttpClientTesting(),
        { provide: CachesService, useValue: { getCache: () => NEVER, getCacheStats, getSubscribers: () => NEVER } },
        {
          provide: ActivatedRoute,
          useValue: {
            snapshot: { paramMap: convertToParamMap({ cache: 'main' }), queryParamMap: convertToParamMap({}) },
            parent: { data: of({ cacheAccess: { cache: { name: 'main', display_name: 'Main', active: true, public: true } } }) },
          },
        },
        { provide: StarsService, useValue: { starred: () => of(false), set: () => of(true) } },
      ],
    });
    const fixture = TestBed.createComponent(CacheDetailComponent);
    fixture.detectChanges();
    const root = fixture.nativeElement as HTMLElement;

    expect(root.textContent).toContain('Failed to load cache stats');
    expect(root.textContent).not.toContain('Packages stored');
    root.querySelector<HTMLButtonElement>('.gr-load-state__error button')?.click();
    fixture.detectChanges();
    expect(getCacheStats).toHaveBeenCalledTimes(2);
    expect(root.textContent).toContain('Packages stored');
  });
});

describe('CacheDetailComponent empty and inactive caches', () => {
  function render(active: boolean) {
    TestBed.configureTestingModule({
      imports: [CacheDetailComponent],
      providers: [
        provideRouter([]),
        provideHttpClient(),
        provideHttpClientTesting(),
        { provide: CachesService, useValue: { getCache: () => NEVER, getCacheStats: () => of({ total_packages: 0, total_bytes: 0, total_nar_bytes: 0, hours: [] }), getSubscribers: () => NEVER } },
        {
          provide: ActivatedRoute,
          useValue: {
            snapshot: { paramMap: convertToParamMap({ cache: 'main' }), queryParamMap: convertToParamMap({}) },
            parent: { data: of({ cacheAccess: { cache: { name: 'main', display_name: 'Main', active, public: true } } }) },
          },
        },
        { provide: StarsService, useValue: { starred: () => of(false), set: () => of(true) } },
      ],
    });
    const fixture = TestBed.createComponent(CacheDetailComponent);
    fixture.detectChanges();
    return fixture.nativeElement as HTMLElement;
  }

  it('replaces storage and traffic with one line while the cache has no packages', () => {
    const root = render(true);
    expect(root.querySelector('[data-testid="no-packages"]')?.textContent).toBe('No packages yet. See "How to Use This Cache" above.');
    expect(root.querySelector('gr-stat-card')).toBeNull();
    expect(root.querySelector('gr-metric-chart')).toBeNull();
  });

  it('warns about an inactive cache and hides the nix.conf snippet', () => {
    const root = render(false);
    expect(root.querySelector('[data-testid="inactive-cache"]')?.textContent).toContain('This cache is inactive.');
    expect(root.textContent).not.toContain('How to Use This Cache');
    expect(root.querySelector('#cache-conf')).toBeNull();
  });
});

describe('CacheDetailComponent header', () => {
  function render(authenticated: boolean) {
    const starred = vi.fn(() => of(true));
    TestBed.configureTestingModule({
      imports: [CacheDetailComponent],
      providers: [
        provideRouter([]),
        provideHttpClient(),
        provideHttpClientTesting(),
        {
          provide: CachesService,
          useValue: {
            getCache: () => NEVER,
            getCacheStats: () => NEVER,
            getSubscribers: () => of({ projects: [{ project: 'acme', project_display_name: 'Acme', mode: 'ReadWrite' }], hidden: 2 }),
          },
        },
        { provide: ActivatedRoute, useValue: { snapshot: { paramMap: convertToParamMap({ cache: 'main' }), queryParamMap: convertToParamMap({}) }, parent: { data: of({ cacheAccess: { cache: { name: 'main', display_name: 'Main', public_key: OWN_KEY, public: true, active: true } } }) } } },
        { provide: AuthService, useValue: { isAuthenticated: () => authenticated, user: () => null } },
        { provide: StarsService, useValue: { starred, set: () => of(true) } },
      ],
    });
    const fixture = TestBed.createComponent(CacheDetailComponent);
    fixture.detectChanges();
    return { root: fixture.nativeElement as HTMLElement, starred };
  }

  it('stars the cache from its header', () => {
    const { root, starred } = render(true);
    expect(starred).toHaveBeenCalledWith({ kind: 'cache', cache: 'main' });
    expect(root.querySelector('gr-star-button button')!.getAttribute('aria-pressed')).toBe('true');
  });

  it('shows no star to a guest', () => {
    expect(render(false).root.querySelector('gr-star-button')).toBeNull();
  });

  it('show projects that use the cache as links and omit private projects', () => {
    const { root } = render(false);
    const section = root.querySelector('.used-by')!;
    expect(section.querySelector('.row-link')?.getAttribute('href')).toBe('/project/acme');
    expect(section.textContent).toContain('Read & Write');
    expect(section.textContent).not.toContain('private');
    expect(section.textContent).not.toContain('Use with project');
  });
});
