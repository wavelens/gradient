/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { signal } from '@angular/core';
import { of } from 'rxjs';
import { AuthService } from './auth.service';
import { PendingUserDecisionsService } from './pending-invites.service';
import { UserService } from './user.service';

const DECISIONS = {
  invites: [{ token: 't1' }],
  subscription_requests: [{ cache: 'main', project: 'shop' }, { cache: 'main', project: 'docs' }],
  team_requests: [{ id: 'r1' }],
};

function setup() {
  const signedIn = signal(false);
  const getRequestsAwaitingUserDecision = vi.fn(() => of(DECISIONS));
  TestBed.configureTestingModule({
    providers: [
      { provide: AuthService, useValue: { isAuthenticated: signedIn } },
      { provide: UserService, useValue: { getRequestsAwaitingUserDecision } },
    ],
  });
  return { signedIn, getRequestsAwaitingUserDecision, decisions: TestBed.inject(PendingUserDecisionsService) };
}

describe('PendingUserDecisionsService', () => {
  it('counts every request kind once someone signs in and forgets them on sign out', () => {
    const { signedIn, getRequestsAwaitingUserDecision, decisions } = setup();
    TestBed.tick();
    expect(getRequestsAwaitingUserDecision).not.toHaveBeenCalled();

    signedIn.set(true);
    TestBed.tick();
    expect(decisions.count()).toBe(4);
    expect(decisions.invites().length).toBe(1);

    signedIn.set(false);
    TestBed.tick();
    expect(decisions.count()).toBe(0);
  });

  it('lowers the count when a handled request is removed', () => {
    const { signedIn, decisions } = setup();
    signedIn.set(true);
    TestBed.tick();
    decisions.removeHandledItemsFromPendingDecisions((all) => ({ ...all, team_requests: [] }));
    expect(decisions.count()).toBe(3);
  });
});
