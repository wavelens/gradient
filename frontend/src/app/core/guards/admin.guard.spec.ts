/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { ActivatedRouteSnapshot, RedirectCommand, Router, RouterStateSnapshot, provideRouter } from '@angular/router';
import { type Observable, Subject, of } from 'rxjs';
import { AuthService } from '@core/services/auth.service';
import { adminGuard } from './admin.guard';

function run(superuser: boolean | null, initialized$: Observable<boolean> = of(true)): Observable<unknown> {
  TestBed.configureTestingModule({
    providers: [
      provideRouter([]),
      { provide: AuthService, useValue: { initialized$, user: () => (superuser === null ? null : { superuser }) } },
    ],
  });
  const result = TestBed.runInInjectionContext(() =>
    adminGuard({} as ActivatedRouteSnapshot, { url: '/admin/webhooks' } as RouterStateSnapshot),
  );
  return result as Observable<unknown>;
}

function decision(result: Observable<unknown>): unknown[] {
  const seen: unknown[] = [];
  result.subscribe((value) => seen.push(value));
  return seen;
}

describe('adminGuard', () => {
  it('Superuser can pass', () => {
    expect(decision(run(true))).toEqual([true]);
  });

  it('All other users go to the "Admin Only" error page, with the URL unchanged.', () => {
    for (const superuser of [false, null]) {
      TestBed.resetTestingModule();
      const [result] = decision(run(superuser));
      expect(result).toBeInstanceOf(RedirectCommand);
      const redirect = result as RedirectCommand;
      expect(TestBed.inject(Router).serializeUrl(redirect.redirectTo as never)).toBe('/error/403?only=admin&from=%2Fadmin%2Fwebhooks');
      expect(redirect.navigationBehaviorOptions?.browserUrl).toBe('/admin/webhooks');
    }
  });

  it('Wait for the first session check, then decide.', () => {
    const initialized = new Subject<boolean>();
    const seen = decision(run(true, initialized));
    expect(seen).toEqual([]);

    initialized.next(true);
    expect(seen).toEqual([true]);
  });
});
