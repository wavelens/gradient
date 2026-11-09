/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { HttpContext, HttpErrorResponse, HttpHeaders, HttpRequest, HttpResponse } from '@angular/common/http';
import { TestBed } from '@angular/core/testing';
import { Router, UrlTree, provideRouter } from '@angular/router';
import { MessageService } from '@gradient/ui/ui';
import { Observable, defer, firstValueFrom, of, throwError } from 'rxjs';
import { AuthService } from '@core/services/auth.service';
import { errorInterceptor, MAX_THROTTLE_WAIT_MS } from './error.interceptor';
import { asSessionProbe } from './request-context';
import { PageRenderTracker } from './page-render-tracker.service';

interface Fixture {
  router: Router;
  navigate: ReturnType<typeof vi.spyOn>;
  auth: { sessionLost: ReturnType<typeof vi.fn>; isAuthenticated: () => boolean; stillSignedIn: () => Observable<boolean> };
  toasts: MessageService;
}

function setup(auth: Partial<Fixture['auth']> = {}, rendering = true): Fixture {
  const fake = { sessionLost: vi.fn(), isAuthenticated: () => true, stillSignedIn: () => of(true), ...auth };
  TestBed.configureTestingModule({
    providers: [
      provideRouter([]),
      { provide: AuthService, useValue: fake },
      { provide: PageRenderTracker, useValue: { isPageStillRendering: () => rendering } },
    ],
  });
  const router = TestBed.inject(Router);
  const navigate = vi.spyOn(router, 'navigate').mockResolvedValue(true);
  return { router, navigate, auth: fake, toasts: TestBed.inject(MessageService) };
}

function send(
  answers: () => Observable<HttpResponse<unknown>>,
  init: { method?: string; context?: HttpContext } = {},
): Promise<unknown> {
  const method = init.method ?? 'GET';
  const request = method === 'GET'
    ? new HttpRequest('GET', '/api/v1/x', { context: init.context })
    : new HttpRequest(method as 'POST', '/api/v1/x', null, { context: init.context });
  return TestBed.runInInjectionContext(() => firstValueFrom(errorInterceptor(request, answers)));
}

const failWith = (status: number, headers?: HttpHeaders) => () => throwError(() => new HttpErrorResponse({ status, headers }));

describe('errorInterceptor on a lost session', () => {
  it('hands a 401 to the session with the page still being navigated to', async () => {
    const { router, auth } = setup();
    const target = router.parseUrl('/project/acme/log/e1?build=b1') as UrlTree;
    vi.spyOn(router, 'currentNavigation').mockReturnValue({ finalUrl: target } as ReturnType<Router['currentNavigation']>);
    await send(failWith(401)).catch(() => undefined);
    expect(auth.sessionLost).toHaveBeenCalledWith('/project/acme/log/e1?build=b1');
  });

  it('hands a 401 to the session with the current page when nothing is navigating', async () => {
    const { router, auth } = setup();
    vi.spyOn(router, 'url', 'get').mockReturnValue('/caches');
    await send(failWith(401)).catch(() => undefined);
    expect(auth.sessionLost).toHaveBeenCalledWith('/caches');
  });

  it('leaves a rejected session probe to the route guards', async () => {
    const { auth } = setup();
    await send(failWith(401), asSessionProbe()).catch(() => undefined);
    expect(auth.sessionLost).not.toHaveBeenCalled();
  });

  it('treats a 403 as a lost session once the server no longer knows the user', async () => {
    const { auth } = setup({ stillSignedIn: () => of(false) });
    await send(failWith(403)).catch(() => undefined);
    expect(auth.sessionLost).toHaveBeenCalled();
  });

  it('keeps the session on a 403 for a missing permission', async () => {
    const { auth } = setup({ stillSignedIn: () => of(true) });
    await send(failWith(403)).catch(() => undefined);
    expect(auth.sessionLost).not.toHaveBeenCalled();
  });

  it('ignores a 403 for an anonymous visitor', async () => {
    const stillSignedIn = vi.fn(() => of(false));
    const { auth } = setup({ isAuthenticated: () => false, stillSignedIn });
    await send(failWith(403)).catch(() => undefined);
    expect(stillSignedIn).not.toHaveBeenCalled();
    expect(auth.sessionLost).not.toHaveBeenCalled();
  });
});

describe('errorInterceptor on 429', () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  function throttled(times: number, retryAfter = '1', method = 'GET') {
    let calls = 0;
    const answers = () =>
      defer(() =>
        ++calls <= times
          ? throwError(() => new HttpErrorResponse({ status: 429, headers: new HttpHeaders({ 'retry-after': retryAfter }) }))
          : of(new HttpResponse({ status: 200 })),
      );
    const result = send(answers, { method });
    return { result, calls: () => calls };
  }

  it('sends a throttled read again after the wait the server named', async () => {
    const { toasts } = setup();
    const { result, calls } = throttled(1);
    await vi.advanceTimersByTimeAsync(999);
    expect(calls()).toBe(1);
    expect(toasts.messages()[0]?.detail).toBe('Retry in 1 s.');
    await vi.advanceTimersByTimeAsync(1);
    expect(((await result) as HttpResponse<unknown>).status).toBe(200);
    expect(calls()).toBe(2);
  });

  it('shows the 429 page at once when the server asks for a wait beyond the cap', async () => {
    const { navigate, toasts } = setup();
    const { result, calls } = throttled(1, String(MAX_THROTTLE_WAIT_MS / 1000 + 1));
    expect(await result.catch((e: HttpErrorResponse) => e.status)).toBe(429);
    expect(calls()).toBe(1);
    expect(toasts.messages()).toEqual([]);
    expect(navigate).toHaveBeenCalledWith(['/error/429'], expect.objectContaining({ skipLocationChange: true }));
  });

  it('names the full server wait when a write is throttled for long', async () => {
    const { toasts } = setup();
    const { result } = throttled(Infinity, '3600', 'POST');
    await result.catch(() => undefined);
    expect(toasts.messages()[0]?.detail).toBe('Try again in 3600 seconds.');
  });

  it('shows the 429 page when the server keeps throttling a read', async () => {
    const { navigate } = setup();
    const { result, calls } = throttled(Infinity);
    const settled = result.catch((e: HttpErrorResponse) => e.status);
    await vi.advanceTimersByTimeAsync(60_000);
    expect(await settled).toBe(429);
    expect(calls()).toBe(4);
    expect(navigate).toHaveBeenCalledWith(['/error/429'], expect.objectContaining({ skipLocationChange: true }));
  });

  it('keeps the page and stays silent when a read sent after the page rendered stays throttled', async () => {
    const { navigate, toasts } = setup({}, false);
    let calls = 0;
    const answers = () => defer(() => (++calls, throwError(() => new HttpErrorResponse({ status: 429, headers: new HttpHeaders({ 'retry-after': '2' }) }))));
    const settled = send(answers).catch((e: HttpErrorResponse) => e.status);
    await vi.advanceTimersByTimeAsync(10_000);
    expect(await settled).toBe(429);
    expect(calls).toBe(4);
    expect(navigate).not.toHaveBeenCalled();
    expect(toasts.messages()).toEqual([]);
  });

  it('never resends a write and keeps the page', async () => {
    const { navigate, toasts } = setup();
    const { result, calls } = throttled(Infinity, '5', 'POST');
    expect(await result.catch((e: HttpErrorResponse) => e.status)).toBe(429);
    expect(calls()).toBe(1);
    expect(navigate).not.toHaveBeenCalled();
    expect(toasts.messages()[0]?.detail).toBe('Try again in 5 seconds.');
  });
});

describe('errorInterceptor on an unavailable server', () => {
  it('renders the error page in place and returns to the page being opened', async () => {
    const { router, navigate } = setup();
    const target = router.parseUrl('/board/workers') as UrlTree;
    vi.spyOn(router, 'currentNavigation').mockReturnValue({ finalUrl: target } as ReturnType<Router['currentNavigation']>);
    await send(failWith(503)).catch(() => undefined);
    expect(navigate).toHaveBeenCalledWith(['/error/503'], { queryParams: { from: '/board/workers' }, skipLocationChange: true });
  });

  it('keeps the page for a failing poll sent after the page rendered', async () => {
    const { navigate } = setup({}, false);
    await send(failWith(503)).catch(() => undefined);
    expect(navigate).not.toHaveBeenCalled();
  });

  it('keeps the page for a failing write', async () => {
    const { navigate } = setup();
    await send(failWith(0), { method: 'DELETE' }).catch(() => undefined);
    expect(navigate).not.toHaveBeenCalled();
  });
});
