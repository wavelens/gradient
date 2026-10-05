/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { HttpErrorResponse, HttpHeaders, HttpRequest, HttpResponse } from '@angular/common/http';
import { TestBed } from '@angular/core/testing';
import { Router, UrlTree, provideRouter } from '@angular/router';
import { defer, firstValueFrom, of, throwError } from 'rxjs';
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

describe('errorInterceptor on 429', () => {
  beforeEach(() => {
    TestBed.configureTestingModule({ providers: [provideRouter([])] });
    vi.useFakeTimers();
  });
  afterEach(() => vi.useRealTimers());

  function throttledFor(times: number) {
    let calls = 0;
    const result = TestBed.runInInjectionContext(() =>
      firstValueFrom(
        errorInterceptor(new HttpRequest('GET', '/api/v1/user'), () =>
          defer(() =>
            ++calls <= times
              ? throwError(() => new HttpErrorResponse({ status: 429, headers: new HttpHeaders({ 'retry-after': '1' }) }))
              : of(new HttpResponse({ status: 200 })),
          ),
        ),
      ),
    );
    return { result, calls: () => calls };
  }

  it('sends a throttled request again after the wait the server named', async () => {
    const { result, calls } = throttledFor(1);
    await vi.advanceTimersByTimeAsync(999);
    expect(calls()).toBe(1);
    await vi.advanceTimersByTimeAsync(1);
    expect(((await result) as HttpResponse<unknown>).status).toBe(200);
    expect(calls()).toBe(2);
  });

  it('passes the 429 on and shows its error page when the server keeps throttling', async () => {
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigate').mockResolvedValue(true);
    const { result, calls } = throttledFor(Infinity);
    const settled = result.catch((e: HttpErrorResponse) => e.status);
    await vi.advanceTimersByTimeAsync(60_000);
    expect(await settled).toBe(429);
    expect(calls()).toBe(4);
    expect(navigate).toHaveBeenCalledWith(['/error/429'], expect.objectContaining({ skipLocationChange: true }));
  });
});
