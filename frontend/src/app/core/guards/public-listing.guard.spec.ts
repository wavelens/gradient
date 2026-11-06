/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { Router, RouterStateSnapshot, ActivatedRouteSnapshot, provideRouter } from '@angular/router';
import { Subject, firstValueFrom, isObservable, of } from 'rxjs';
import { publicListingGuard } from './public-listing.guard';
import { AuthService } from '@core/services/auth.service';
import { ConfigService } from '@core/services/config.service';

async function run(publicListing: boolean) {
  TestBed.configureTestingModule({
    providers: [
      provideRouter([]),
      { provide: ConfigService, useValue: { publicListing } },
      {
        provide: AuthService,
        useValue: {
          initialized$: of(true),
          resolveSession: () => of(false),
          serverUnreachable: () => false,
        },
      },
    ],
  });
  const navigate = vi.spyOn(TestBed.inject(Router), 'navigate').mockResolvedValue(true);
  const result = TestBed.runInInjectionContext(() =>
    publicListingGuard({} as ActivatedRouteSnapshot, { url: '/projects' } as RouterStateSnapshot)
  );
  const allowed = isObservable(result) ? await firstValueFrom(result) : await result;
  return { allowed, navigate };
}

describe('publicListingGuard', () => {
  it('lets a logged-out visitor browse the lists while public listing is on', async () => {
    const { allowed, navigate } = await run(true);
    expect(allowed).toBe(true);
    expect(navigate).not.toHaveBeenCalled();
  });

  it('waits for the session check before a list page reads who is signed in', () => {
    const initialized = new Subject<boolean>();
    TestBed.configureTestingModule({
      providers: [
        provideRouter([]),
        { provide: ConfigService, useValue: { publicListing: true } },
        { provide: AuthService, useValue: { initialized$: initialized } },
      ],
    });
    const result = TestBed.runInInjectionContext(() =>
      publicListingGuard({} as ActivatedRouteSnapshot, { url: '/caches' } as RouterStateSnapshot)
    );
    let allowed: unknown;
    if (isObservable(result)) result.subscribe((value) => (allowed = value));
    expect(allowed).toBeUndefined();
    initialized.next(true);
    expect(allowed).toBe(true);
  });

  it('sends a logged-out visitor to login while public listing is off', async () => {
    const { allowed, navigate } = await run(false);
    expect(allowed).toBe(false);
    expect(navigate).toHaveBeenCalledWith(['/account/login'], {
      queryParams: { next: '/projects' },
    });
  });
});
