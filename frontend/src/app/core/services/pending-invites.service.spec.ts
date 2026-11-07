/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { signal } from '@angular/core';
import { of } from 'rxjs';
import { AuthService } from './auth.service';
import { PendingInvitesService } from './pending-invites.service';
import { UserService } from './user.service';

describe('PendingInvitesService', () => {
  it('loads the invites once someone signs in and forgets them on sign out', () => {
    const signedIn = signal(false);
    const getInvites = vi.fn(() => of([{ token: 't1' }, { token: 't2' }]));
    TestBed.configureTestingModule({
      providers: [
        { provide: AuthService, useValue: { isAuthenticated: signedIn } },
        { provide: UserService, useValue: { getInvites } },
      ],
    });
    const invites = TestBed.inject(PendingInvitesService);
    TestBed.tick();
    expect(getInvites).not.toHaveBeenCalled();

    signedIn.set(true);
    TestBed.tick();
    expect(invites.count()).toBe(2);

    signedIn.set(false);
    TestBed.tick();
    expect(invites.count()).toBe(0);
  });
});
