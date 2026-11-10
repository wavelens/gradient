/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { of, throwError } from 'rxjs';
import { AuthService } from '@core/services/auth.service';
import { UserService } from '@core/services/user.service';
import { getDisplayNameWithOwnerTeam, loadSignedInUserAccessSummary } from './signed-in-user-access-summary';

function load(signedIn: boolean, request: () => unknown) {
  const getProjectsCachesAndTeamsCurrentUserCanReach = vi.fn(request);
  TestBed.configureTestingModule({
    providers: [
      { provide: AuthService, useValue: { isAuthenticated: () => signedIn } },
      { provide: UserService, useValue: { getProjectsCachesAndTeamsCurrentUserCanReach } },
    ],
  });
  const summary = TestBed.runInInjectionContext(() => loadSignedInUserAccessSummary());
  return { summary, getProjectsCachesAndTeamsCurrentUserCanReach };
}

describe('loadSignedInUserAccessSummary', () => {
  it('loads the access rows of the signed-in user', () => {
    const rows = { superuser: false, projects: [{ name: 'acme', display_name: 'Acme', role: 'View', reasons: [] }], caches: [], teams: [] };
    expect(load(true, () => of(rows)).summary()).toEqual(rows);
  });

  it('skips the request for anonymous visitors', () => {
    const { summary, getProjectsCachesAndTeamsCurrentUserCanReach } = load(false, () => of(null));
    expect(summary()).toBeNull();
    expect(getProjectsCachesAndTeamsCurrentUserCanReach).not.toHaveBeenCalled();
  });

  it('stays empty when the request fails', () => {
    expect(load(true, () => throwError(() => new Error('down'))).summary()).toBeNull();
  });
});

describe('getDisplayNameWithOwnerTeam', () => {
  it('appends the owner team display name', () => {
    expect(getDisplayNameWithOwnerTeam('Acme', { name: 'infra', display_name: 'Infra' })).toBe('Acme · Infra');
  });

  it('keeps the name without an owner team', () => {
    expect(getDisplayNameWithOwnerTeam('Acme', null)).toBe('Acme');
  });
});
