/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { environment } from '@environments/environment';
import { ServerConfig } from '@core/models';
import { ConfigService } from './config.service';

const configUrl = `${environment.apiUrl}/config`;

const serverConfig = {
  version: '2.0.1',
  oidc_enabled: true,
  login_providers: [],
  password_login_enabled: false,
  registration_enabled: false,
  email_verification_enabled: false,
  smtp_enabled: false,
  exec_enabled: true,
  public_listing: false,
} as unknown as ServerConfig;

describe('ConfigService', () => {
  let service: ConfigService;
  let http: HttpTestingController;

  beforeEach(() => {
    vi.useFakeTimers();
    TestBed.configureTestingModule({ providers: [provideHttpClient(), provideHttpClientTesting()] });
    service = TestBed.inject(ConfigService);
    http = TestBed.inject(HttpTestingController);
  });

  afterEach(() => {
    http.verify();
    vi.useRealTimers();
  });

  const failOnce = () => http.expectOne(configUrl).flush(null, { status: 503, statusText: 'Service Unavailable' });

  it('Apply server configuration', async () => {
    const loading = service.load();
    http.expectOne(configUrl).flush({ error: false, message: serverConfig });
    await loading;

    expect(service.oidcEnabled).toBe(true);
    expect(service.passwordLoginEnabled).toBe(false);
    expect(service.registrationDisabled).toBe(true);
    expect(service.execEnabled).toBe(true);
    expect(service.publicListing).toBe(false);
    expect(service.serverConfigFetchFailedAfterRetries()).toBe(false);
  });

  it('Retry a failed fetch and use the response that arrived', async () => {
    const loading = service.load();
    failOnce();
    await vi.advanceTimersByTimeAsync(500);
    http.expectOne(configUrl).flush({ error: false, message: serverConfig });
    await loading;

    expect(service.passwordLoginEnabled).toBe(false);
    expect(service.serverConfigFetchFailedAfterRetries()).toBe(false);
  });

  it('Report the failure after all retries fail.', async () => {
    const loading = service.load();
    failOnce();
    for (const delay of [500, 1000, 2000]) {
      await vi.advanceTimersByTimeAsync(delay);
      failOnce();
    }
    await loading;

    expect(service.serverConfigFetchFailedAfterRetries()).toBe(true);
  });

  it('Report failure on a server reply with an error envelope', async () => {
    const loading = service.load();
    http.expectOne(configUrl).flush({ error: true, message: 'broken' });
    await loading;

    expect(service.serverConfigFetchFailedAfterRetries()).toBe(true);
    expect(service.passwordLoginEnabled).toBe(true);
  });
});
