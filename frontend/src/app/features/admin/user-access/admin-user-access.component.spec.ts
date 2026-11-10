/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { ActivatedRoute, Router, convertToParamMap, provideRouter } from '@angular/router';
import { BehaviorSubject, EMPTY, Observable, of } from 'rxjs';
import type { UserAccessSummary } from '@core/models';
import { AdminService } from '@core/services/admin.service';
import { NotifyService } from '@core/services/notify.service';
import { UserService } from '@core/services/user.service';
import { ConfirmRequest, ConfirmService } from '@shared/confirm';
import { AdminUserAccessComponent, formatRevokedAccessCountsText } from './admin-user-access.component';

const summary: UserAccessSummary = {
  superuser: false,
  projects: [{ name: 'acme', display_name: 'Acme', role: 'Write', reasons: [{ kind: 'direct', role: 'Write', applies: true, managed: false }] }],
  caches: [],
  teams: [],
};

function setup(username: string | null, confirmAnswer: 'confirm' | 'cancel' = 'confirm') {
  const params = new BehaviorSubject(convertToParamMap(username ? { username } : {}));
  const admin = {
    getUserAccessToAllProjectsCachesAndTeams: vi.fn(() => of(summary)),
    removeUserMembershipsAndInvitationsAndReturnCounts: vi.fn(() => of({ projects: 2, caches: 1, teams: 0, invitations: 1 })),
  };
  const notify = { success: vi.fn(), failure: vi.fn() };
  const confirm = vi.fn((request: ConfirmRequest): Observable<unknown> => (confirmAnswer === 'confirm' ? request.action() : EMPTY));
  TestBed.configureTestingModule({
    imports: [AdminUserAccessComponent],
    providers: [
      provideRouter([]),
      { provide: ActivatedRoute, useValue: { paramMap: params } },
      { provide: AdminService, useValue: admin },
      { provide: UserService, useValue: { searchUsers: () => of([{ id: 'u1', username: 'carol', name: 'Carol' }]) } },
      { provide: NotifyService, useValue: notify },
      { provide: ConfirmService, useValue: { confirm } },
    ],
  });
  const fixture = TestBed.createComponent(AdminUserAccessComponent);
  fixture.detectChanges();
  const root = fixture.nativeElement as HTMLElement;
  const button = (label: string) =>
    Array.from(root.querySelectorAll<HTMLButtonElement>('button')).find((entry) => entry.querySelector('.gr-button__label')?.textContent?.trim() === label) ?? null;
  return { fixture, root, params, admin, notify, confirm, button };
}

describe('AdminUserAccessComponent', () => {
  it('shows only the user search before a user is chosen', () => {
    const { root, admin, button } = setup(null);
    expect(admin.getUserAccessToAllProjectsCachesAndTeams).not.toHaveBeenCalled();
    expect(root.querySelector('#admin-user-access-username')).not.toBeNull();
    expect(button('Revoke All Access')).toBeNull();
  });

  it('opens the access page of the typed user', () => {
    const { fixture, button } = setup(null);
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigateByUrl').mockResolvedValue(true);
    fixture.componentInstance.typedUsername.set(' carol ');
    button('View access')!.click();
    expect(String(navigate.mock.calls[0][0])).toBe('/admin/users/carol/access');
  });

  it('lists the access of the user from the address and follows a change to another user', () => {
    const { fixture, root, params, admin } = setup('carol');
    expect(admin.getUserAccessToAllProjectsCachesAndTeams).toHaveBeenCalledWith('carol');
    expect(root.querySelector('a[href="/project/acme"]')?.closest('gr-row')?.textContent).toContain('Role Write · Direct');
    params.next(convertToParamMap({ username: 'dave' }));
    fixture.detectChanges();
    expect(admin.getUserAccessToAllProjectsCachesAndTeams).toHaveBeenLastCalledWith('dave');
  });

  it('revokes all access after confirmation, reports the counts and reloads', () => {
    const { fixture, admin, notify, confirm, button } = setup('carol');
    button('Revoke All Access')!.click();
    fixture.detectChanges();
    expect(confirm.mock.calls[0][0]).toEqual(expect.objectContaining({ target: 'carol', severity: 'danger', failure: "Couldn't revoke access" }));
    expect(admin.removeUserMembershipsAndInvitationsAndReturnCounts).toHaveBeenCalledWith('carol');
    expect(notify.success).toHaveBeenCalledWith('Access revoked: 2 projects, 1 cache, 0 teams, 1 invitation');
    expect(admin.getUserAccessToAllProjectsCachesAndTeams).toHaveBeenCalledTimes(2);
  });

  it('revokes nothing when the confirmation is cancelled', () => {
    const { admin, button } = setup('carol', 'cancel');
    button('Revoke All Access')!.click();
    expect(admin.removeUserMembershipsAndInvitationsAndReturnCounts).not.toHaveBeenCalled();
  });

  it('counts each kind of removed access in singular or plural', () => {
    expect(formatRevokedAccessCountsText({ projects: 1, caches: 0, teams: 3, invitations: 0 })).toBe('1 project, 0 caches, 3 teams, 0 invitations');
  });
});
