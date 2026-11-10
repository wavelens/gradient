/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { Router, UrlTree, provideRouter } from '@angular/router';
import { type Observable, of, throwError } from 'rxjs';
import { AuthService } from '@core/services/auth.service';
import { ApiError } from '@core/services/api.service';
import { OidcCallbackComponent } from './oidc-callback.component';

function run(result: Observable<unknown>, storedPath: string | null = null): string {
  if (storedPath !== null) sessionStorage.setItem('oidc_next', storedPath);
  TestBed.configureTestingModule({
    imports: [OidcCallbackComponent],
    providers: [provideRouter([]), { provide: AuthService, useValue: { loginWithCookie: () => result } }],
  });
  const router = TestBed.inject(Router);
  const navigate = vi.spyOn(router, 'navigateByUrl').mockResolvedValue(true);
  TestBed.createComponent(OidcCallbackComponent).detectChanges();
  const target = navigate.mock.calls[0][0];
  return typeof target === 'string' ? target : router.serializeUrl(target as UrlTree);
}

describe('OidcCallbackComponent', () => {
  afterEach(() => sessionStorage.clear());

  it('Open Page Saved Before Sign-On', () => {
    expect(run(of({ username: 'alice' }), '/project/acme?tab=1')).toBe('/project/acme?tab=1');
    expect(sessionStorage.getItem('oidc_next')).toBeNull();
  });

  it('Ignore saved paths that lead outside the app', () => {
    expect(run(of({ username: 'alice' }), '//evil.example')).toBe('/');
    TestBed.resetTestingModule();
    expect(run(of({ username: 'alice' }), '/\\evil.example')).toBe('/');
  });

  it('Explain the missing session cookie on the login page.', () => {
    expect(run(throwError(() => new ApiError('no cookie', 401)))).toBe('/account/login?login_error=session_cookie_missing');
  });

  it('Report other failures on the login page.', () => {
    expect(run(throwError(() => new ApiError('down', 503)), '/projects')).toBe('/account/login?login_error=failed');
    expect(sessionStorage.getItem('oidc_next')).toBeNull();
  });
});
