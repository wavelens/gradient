/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { Router, UrlTree, provideRouter } from '@angular/router';
import { firstValueFrom } from 'rxjs';
import { AuthService } from './auth.service';
import { environment } from '@environments/environment';

const userUrl = `${environment.apiUrl}/user`;

const user = {
  id: 'user-1',
  username: 'gradient',
  name: 'Gradient',
  email: 'gradient@example.invalid',
  superuser: false,
};

describe('AuthService session probe', () => {
  let service: AuthService;
  let httpMock: HttpTestingController;

  function boot(answer: () => void) {
    TestBed.configureTestingModule({
      providers: [provideHttpClient(), provideHttpClientTesting(), provideRouter([])],
    });
    service = TestBed.inject(AuthService);
    httpMock = TestBed.inject(HttpTestingController);
    answer();
  }

  afterEach(() => httpMock.verify());

  it('holds a session the server confirmed', () => {
    boot(() => httpMock.expectOne(userUrl).flush({ error: false, message: user }));
    expect(service.isAuthenticated()).toBe(true);
    expect(service.serverUnreachable()).toBe(false);
  });

  it('treats a rejected probe as logged out', () => {
    boot(() =>
      httpMock.expectOne(userUrl).flush(
        { error: true, message: 'Unauthorized' },
        { status: 401, statusText: 'Unauthorized' }
      )
    );
    expect(service.isAuthenticated()).toBe(false);
    expect(service.serverUnreachable()).toBe(false);
  });

  // A gateway that never reached the backend says nothing about the session:
  // concluding "logged out" here is what put an outage on the login page.
  it('leaves the session unknown when the server is unreachable', () => {
    boot(() =>
      httpMock
        .expectOne(userUrl)
        .flush('Bad Gateway', { status: 502, statusText: 'Bad Gateway' })
    );
    expect(service.serverUnreachable()).toBe(true);
    expect(service.isAuthenticated()).toBe(false);
  });

  it('leaves the session unknown when the server throttles the probe', () => {
    boot(() =>
      httpMock
        .expectOne(userUrl)
        .flush('Too Many Requests', { status: 429, statusText: 'Too Many Requests' })
    );
    expect(service.serverUnreachable()).toBe(true);
  });

  it('re-probes once the server answers again, without a fresh page load', async () => {
    boot(() =>
      httpMock
        .expectOne(userUrl)
        .flush('Bad Gateway', { status: 502, statusText: 'Bad Gateway' })
    );

    const resolved = new Promise<boolean>((done) => service.resolveSession().subscribe(done));
    httpMock.expectOne(userUrl).flush({ error: false, message: user });

    expect(await resolved).toBe(true);
    expect(service.isAuthenticated()).toBe(true);
    expect(service.serverUnreachable()).toBe(false);
  });

  it('forgets an expired session and returns to the page after sign-in', () => {
    boot(() => httpMock.expectOne(userUrl).flush({ error: false, message: user }));
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigate').mockResolvedValue(true);
    service.sessionLost('/teams');
    expect(service.isAuthenticated()).toBe(false);
    httpMock.expectOne(`${environment.apiUrl}/auth/logout`).flush({ error: false, message: 'ok' });
    expect(navigate).toHaveBeenCalledWith(['/account/login'], { queryParams: { next: '/teams', reason: 'expired' } });
  });

  it('sends a visitor without a session to sign in without claiming it expired', () => {
    boot(() => httpMock.expectOne(userUrl).flush({ error: true, message: 'no' }, { status: 403, statusText: 'Forbidden' }));
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigate').mockResolvedValue(true);
    service.sessionLost('/settings/keys');
    httpMock.expectNone(`${environment.apiUrl}/auth/logout`);
    expect(navigate).toHaveBeenCalledWith(['/account/login'], { queryParams: { next: '/settings/keys' } });
  });

  it('marks the sign-in page as expired when a cookie rejected on page load fails again later', () => {
    boot(() => httpMock.expectOne(userUrl).flush({ error: true, message: 'no' }, { status: 401, statusText: 'Unauthorized' }));
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigate').mockResolvedValue(true);
    service.sessionLost('/project/p');
    httpMock.expectNone(`${environment.apiUrl}/auth/logout`);
    expect(navigate).toHaveBeenCalledWith(['/account/login'], { queryParams: { next: '/project/p', reason: 'expired' } });
  });

  it('marks the sign-in link as expired only when the server rejected a token on page load', () => {
    const signInFor = (status: number) => {
      TestBed.resetTestingModule();
      boot(() => httpMock.expectOne(userUrl).flush({ error: true, message: 'no' }, { status, statusText: 'No' }));
      return TestBed.inject(Router).serializeUrl(service.getSignInPageUrl('/teams'));
    };
    expect(signInFor(401)).toBe('/account/login?next=%2Fteams&reason=expired');
    expect(signInFor(403)).toBe('/account/login?next=%2Fteams');
  });

  it('redirects once while the sign-in page is already on its way', () => {
    boot(() => httpMock.expectOne(userUrl).flush({ error: false, message: user }));
    const router = TestBed.inject(Router);
    const navigate = vi.spyOn(router, 'navigate').mockResolvedValue(true);
    vi.spyOn(router, 'currentNavigation').mockReturnValue({
      finalUrl: router.parseUrl('/account/login?next=%2Fteams'),
    } as ReturnType<Router['currentNavigation']>);
    service.sessionLost('/teams');
    httpMock.expectOne(`${environment.apiUrl}/auth/logout`).flush({ error: false, message: 'ok' });
    expect(navigate).not.toHaveBeenCalled();
  });

  it('forgets a deleted account without a logout call and tells other tabs', () => {
    boot(() => httpMock.expectOne(userUrl).flush({ error: false, message: user }));
    const router = TestBed.inject(Router);
    const navigate = vi.spyOn(router, 'navigateByUrl').mockResolvedValue(true);
    service.forgetDeletedAccountAndGoToSignInPage();
    expect(service.isAuthenticated()).toBe(false);
    httpMock.expectNone(`${environment.apiUrl}/auth/logout`);
    expect(localStorage.getItem('gradient-signed-out')).toBeTruthy();
    expect(router.serializeUrl(navigate.mock.calls[0][0] as UrlTree)).toBe('/account/login?reason=deleted');
  });

  it('signs out when another tab signs out', () => {
    boot(() => httpMock.expectOne(userUrl).flush({ error: false, message: user }));
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigate').mockResolvedValue(true);
    window.dispatchEvent(new StorageEvent('storage', { key: 'gradient-signed-out', newValue: '1' }));
    expect(service.isAuthenticated()).toBe(false);
    expect(navigate).toHaveBeenCalledWith(['/account/login'], {});
  });

  it('asks the server once for overlapping session checks', async () => {
    boot(() => httpMock.expectOne(userUrl).flush({ error: false, message: user }));
    const first = firstValueFrom(service.stillSignedIn());
    const second = firstValueFrom(service.stillSignedIn());
    httpMock.expectOne(userUrl).flush({ error: true, message: 'no' }, { status: 401, statusText: 'Unauthorized' });
    expect(await first).toBe(false);
    expect(await second).toBe(false);
  });

  it('does not re-probe when the last answer was conclusive', async () => {
    boot(() => httpMock.expectOne(userUrl).flush({ error: false, message: user }));
    const resolved = new Promise<boolean>((done) => service.resolveSession().subscribe(done));
    httpMock.expectNone(userUrl);
    expect(await resolved).toBe(true);
  });
});
