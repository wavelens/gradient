/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { of } from 'rxjs';
import { CacheDetailComponent } from './cache-detail.component';
import { CachesService } from '@core/services/caches.service';

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
