/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { Router, RouterStateSnapshot, ActivatedRouteSnapshot } from '@angular/router';
import { provideRouter } from '@angular/router';
import { Observable, firstValueFrom, isObservable, of } from 'rxjs';
import { authGuard, redirectSignedInUserToDashboardGuard } from './auth.guard';
import { AuthService } from '@core/services/auth.service';

interface StubAuth {
  initialized$: Observable<boolean>;
  resolveSession: () => Observable<boolean>;
  serverUnreachable: () => boolean;
}

async function run(auth: StubAuth) {
  TestBed.configureTestingModule({
    providers: [provideRouter([]), { provide: AuthService, useValue: auth }],
  });
  const router = TestBed.inject(Router);
  const navigate = vi.spyOn(router, 'navigate').mockResolvedValue(true);
  const result = TestBed.runInInjectionContext(() =>
    authGuard({} as ActivatedRouteSnapshot, { url: '/dashboard' } as RouterStateSnapshot)
  );
  const allowed = isObservable(result) ? await firstValueFrom(result) : await result;
  return { allowed, navigate };
}

describe('authGuard', () => {
  it('lets an authenticated caller through', async () => {
    const { allowed, navigate } = await run({
      initialized$: of(true),
      resolveSession: () => of(true),
      serverUnreachable: () => false,
    });
    expect(allowed).toBe(true);
    expect(navigate).not.toHaveBeenCalled();
  });

  it('sends a logged-out caller to login, carrying where they were going', async () => {
    const { allowed, navigate } = await run({
      initialized$: of(true),
      resolveSession: () => of(false),
      serverUnreachable: () => false,
    });
    expect(allowed).toBe(false);
    expect(navigate).toHaveBeenCalledWith(['/account/login'], {
      queryParams: { next: '/dashboard' },
    });
  });

  // The outage page is already up: sending the caller to login instead would
  // claim a session ended that nothing ever checked.
  it('cancels the navigation rather than logging out when the server is unreachable', async () => {
    const { allowed, navigate } = await run({
      initialized$: of(true),
      resolveSession: () => of(false),
      serverUnreachable: () => true,
    });
    expect(allowed).toBe(false);
    expect(navigate).not.toHaveBeenCalled();
  });
});

describe('redirectSignedInUserToDashboardGuard', () => {
  it('sends a signed-in user from the register form to the dashboard', async () => {
    const check = async (signedIn: boolean) => {
      TestBed.resetTestingModule();
      TestBed.configureTestingModule({
        providers: [provideRouter([]), { provide: AuthService, useValue: { initialized$: of(true), isAuthenticated: () => signedIn } }],
      });
      const result = TestBed.runInInjectionContext(() =>
        redirectSignedInUserToDashboardGuard({} as ActivatedRouteSnapshot, { url: '/account/register' } as RouterStateSnapshot),
      );
      return isObservable(result) ? firstValueFrom(result) : result;
    };
    expect(String(await check(true))).toBe('/');
    expect(await check(false)).toBe(true);
  });
});
