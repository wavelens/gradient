/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { HttpErrorResponse, HttpRequest } from '@angular/common/http';
import { TestBed } from '@angular/core/testing';
import { Router, UrlTree, provideRouter } from '@angular/router';
import { firstValueFrom, throwError } from 'rxjs';
import { errorInterceptor } from './error.interceptor';

function unauthorized(): Promise<unknown> {
  return TestBed.runInInjectionContext(() =>
    firstValueFrom(
      errorInterceptor(new HttpRequest('GET', '/api/v1/x'), () =>
        throwError(() => new HttpErrorResponse({ status: 401 })),
      ),
    ),
  ).catch(() => undefined);
}

describe('errorInterceptor on 401', () => {
  let router: Router;
  let navigate: ReturnType<typeof vi.spyOn>;

  beforeEach(() => {
    TestBed.configureTestingModule({ providers: [provideRouter([])] });
    router = TestBed.inject(Router);
    navigate = vi.spyOn(router, 'navigate').mockResolvedValue(true);
  });

  /// A deep link opened while signed out 401s before the first navigation lands,
  /// when `router.url` still reads `/`; the login must return to the link.
  it('returns to the page still being navigated to', async () => {
    const target = router.parseUrl('/project/acme/log/e1?build=b1') as UrlTree;
    vi.spyOn(router, 'currentNavigation').mockReturnValue({ finalUrl: target } as ReturnType<Router['currentNavigation']>);
    await unauthorized();
    expect(navigate).toHaveBeenCalledWith(['/account/login'], { queryParams: { next: '/project/acme/log/e1?build=b1' } });
  });

  it('returns to the current page when nothing is navigating', async () => {
    vi.spyOn(router, 'currentNavigation').mockReturnValue(null);
    vi.spyOn(router, 'url', 'get').mockReturnValue('/caches');
    await unauthorized();
    expect(navigate).toHaveBeenCalledWith(['/account/login'], { queryParams: { next: '/caches' } });
  });

  it('never loops back to an account page', async () => {
    vi.spyOn(router, 'currentNavigation').mockReturnValue(null);
    vi.spyOn(router, 'url', 'get').mockReturnValue('/account/login');
    await unauthorized();
    expect(navigate).toHaveBeenCalledWith(['/account/login'], {});
  });
});
