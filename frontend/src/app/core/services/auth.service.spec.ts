/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { provideRouter } from '@angular/router';
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

  it('does not re-probe when the last answer was conclusive', async () => {
    boot(() => httpMock.expectOne(userUrl).flush({ error: false, message: user }));
    const resolved = new Promise<boolean>((done) => service.resolveSession().subscribe(done));
    httpMock.expectNone(userUrl);
    expect(await resolved).toBe(true);
  });
});
