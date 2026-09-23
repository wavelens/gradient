/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { NEVER, of } from 'rxjs';
import { CacheDetailComponent } from './cache-detail.component';
import { CachesService } from '@core/services/caches.service';
import { AuthService } from '@core/services/auth.service';
import { StarsService } from '@core/services/stars.service';

const OWN_KEY = 'public.gradient.ci-main:74DUi4Ye579gUqzH4ziL9IyiJBlDpMRn9MBN8oNan9M=';
const UPSTREAM_KEY = 'cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY=';

describe('CacheDetailComponent cache usage instructions', () => {
  function setup() {
    const caches = {
      getCache: vi.fn().mockReturnValue(of({ name: 'main', public_key: OWN_KEY, public: true })),
      getCacheStats: vi.fn().mockReturnValue(of(null)),
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
          useValue: { snapshot: { paramMap: convertToParamMap({ cache: 'main' }) } },
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
  // one key is all a client needs. Listing the upstreams' keys made every user
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
    const component = setup();
    component.cache.set(null);

    expect(component.trustedPublicKeys()).toEqual([]);
    expect(component.nixConfSnippet()).toContain('<unavailable>');
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
            getCache: () => of({ name: 'main', display_name: 'Main', public_key: OWN_KEY, public: true, active: true }),
            getCacheStats: () => NEVER,
          },
        },
        { provide: ActivatedRoute, useValue: { snapshot: { paramMap: convertToParamMap({ cache: 'main' }) } } },
        { provide: AuthService, useValue: { isAuthenticated: () => authenticated } },
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
});
