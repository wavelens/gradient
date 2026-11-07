/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { EMPTY, Observable, Subject, of, throwError } from 'rxjs';
import { TeamMembersComponent } from './team-members.component';
import { TeamsService } from '@core/services/teams.service';
import { NotifyService } from '@core/services/notify.service';
import { AuthService } from '@core/services/auth.service';
import { ApiError } from '@core/services/api.service';
import { ConfirmRequest, ConfirmService } from '@shared/confirm';
import { PendingInvitationItem, TeamResponse, TeamMemberItem } from '@core/models';

const team: TeamResponse = {
  id: 't1',
  name: 'platform',
  display_name: 'Platform',
  managed: false,
  role: 'admin',
  oidc_group: null,
  scim_group: null,
  new_project_users: false,
  new_project_workers: false,
  new_project_role: null,
};

const members: TeamMemberItem[] = [
  { user: 'alice', name: 'Alice', role: 'admin', source: 'api' },
  { user: 'bob', name: 'Bob', role: 'member', source: 'group' },
  { user: 'dave', name: 'Dave', role: 'member', source: 'state' },
];

function setup(
  removeMember = vi.fn().mockReturnValue(of('User removed')),
  managed = false,
  listMembers = vi.fn().mockReturnValue(of(members)),
  invitations: PendingInvitationItem[] = [],
) {
  const addMember = vi.fn().mockReturnValue(of('Invitation sent'));
  const updateMember = vi.fn().mockReturnValue(of('ok'));
  const revokeInvitation = vi.fn().mockReturnValue(of('ok'));
  const notify = { success: vi.fn(), failure: vi.fn() };
  const confirm = vi.fn((request: ConfirmRequest): Observable<unknown> => request.action());
  TestBed.configureTestingModule({
    imports: [TeamMembersComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      {
        provide: TeamsService,
        useValue: {
          members: listMembers,
          invitations: () => of(invitations),
          addMember,
          updateMember,
          removeMember,
          revokeInvitation,
        },
      },
      { provide: NotifyService, useValue: notify },
      { provide: ConfirmService, useValue: { confirm } },
      { provide: AuthService, useValue: { user: signal({ username: 'alice' }) } },
    ],
  });
  const fixture = TestBed.createComponent(TeamMembersComponent);
  fixture.componentRef.setInput('team', { ...team, managed });
  fixture.detectChanges();
  return { fixture, addMember, updateMember, removeMember, revokeInvitation, notify, confirm };
}

describe('TeamMembersComponent', () => {
  it('shows a failed member load with Try Again instead of No members', () => {
    const listMembers = vi.fn().mockReturnValueOnce(throwError(() => new Error('database is down'))).mockReturnValue(of(members));
    const { fixture } = setup(undefined, false, listMembers);
    const root = fixture.nativeElement as HTMLElement;
    expect(root.textContent).toContain('Failed to load team members');
    expect(root.textContent).not.toContain('No members');
    root.querySelector<HTMLButtonElement>('.gr-load-state__error button')?.click();
    fixture.detectChanges();
    expect(listMembers).toHaveBeenCalledTimes(2);
    expect(root.textContent).toContain('alice');
  });

  it('lists members and marks the ones added by a group or the state', () => {
    const { fixture } = setup();
    const text = (fixture.nativeElement as HTMLElement).textContent || '';
    expect(text).toContain('alice');
    expect(text).toContain('bob');
    expect(text).toContain('Group');
    expect(text).toContain('State');
  });

  it('lets an admin of a state-managed team change every member except the declared ones', () => {
    const { fixture } = setup(undefined, true);
    const element = fixture.nativeElement as HTMLElement;
    const row = (user: string) => Array.from(element.querySelectorAll('gr-row')).find((r) => r.textContent?.includes(user))!;
    expect(element.textContent).toContain('Add Member');
    expect(row('alice').querySelector('gr-select')).not.toBeNull();
    expect(row('dave').querySelector('gr-select')).toBeNull();
    expect(row('dave').querySelector('button')).toBeNull();
  });

  it('invites a user with the chosen role', () => {
    const { fixture, addMember } = setup();
    const component = fixture.componentInstance;
    component.newMember = { user: 'carol', role: 'member' };
    component.addMember();
    expect(addMember).toHaveBeenCalledWith('platform', 'carol', 'member');
  });

  it('asks for a username under the field instead of ignoring the click', () => {
    const { fixture, addMember } = setup();
    fixture.componentInstance.openAdd();
    fixture.detectChanges();
    const dialog = document.querySelector('.gr-dialog')!;
    Array.from(dialog.querySelectorAll<HTMLButtonElement>('.gr-dialog__footer button'))
      .find((b) => b.textContent?.trim() === 'Add')!
      .click();
    fixture.detectChanges();
    expect(dialog.textContent).toContain('Enter a username.');
    expect(addMember).not.toHaveBeenCalled();
  });

  it('opens the add dialog empty after a cancelled attempt', () => {
    const { fixture } = setup();
    const component = fixture.componentInstance;
    component.openAdd();
    component.newMember = { user: 'typed-user', role: 'admin' };
    component.showAdd.set(false);
    component.openAdd();
    expect(component.newMember).toEqual({ user: '', role: 'member' });
  });

  it('shows a failed addition inside the add dialog', () => {
    const { fixture, addMember } = setup();
    addMember.mockReturnValue(throwError(() => new ApiError('User not found', 404)));
    const component = fixture.componentInstance;
    component.openAdd();
    fixture.detectChanges();
    component.newMember = { user: 'nobody', role: 'member' };
    component.addMember();
    fixture.detectChanges();
    expect(document.querySelector('.gr-dialog')?.textContent).toContain('User not found');
    expect((fixture.nativeElement as HTMLElement).textContent).not.toContain('User not found');
  });

  it('keeps the add dialog open and sends one request while the addition runs', () => {
    const { fixture, addMember } = setup();
    addMember.mockReturnValue(new Subject());
    const component = fixture.componentInstance;
    component.openAdd();
    fixture.detectChanges();
    component.newMember = { user: 'carol', role: 'member' };
    component.addMember();
    fixture.detectChanges();
    component.addMember();
    const dialog = document.querySelector('.gr-dialog')!;
    dialog.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', keyCode: 27, bubbles: true }));
    (document.querySelector('.gr-dialog-backdrop') as HTMLElement).click();
    fixture.detectChanges();
    expect(addMember).toHaveBeenCalledTimes(1);
    expect(component.showAdd()).toBe(true);
    expect(dialog.getAttribute('aria-busy')).toBe('true');
  });

  it('reports a refused role change', () => {
    const { fixture, updateMember, notify } = setup();
    const failure = new ApiError('Forbidden', 403);
    updateMember.mockReturnValue(throwError(() => failure));
    fixture.componentInstance.updateRole('bob', 'admin');
    expect(notify.failure).toHaveBeenCalledWith('Role change failed', failure);
  });

  it('asks before removing a member, naming the member, then confirms the removal', () => {
    const { fixture, notify, removeMember, confirm } = setup();
    removeButton(fixture.nativeElement, 'bob').click();
    expect(confirm).toHaveBeenCalledWith(
      expect.objectContaining({ title: 'Remove member', target: 'bob', consequence: 'bob can no longer access this team.' }),
    );
    expect(removeMember).toHaveBeenCalledWith('platform', 'bob');
    expect(notify.success).toHaveBeenCalledWith('Member removed');
  });

  it('warns admins that removing themselves takes away their own access', () => {
    const { fixture, confirm } = setup();
    removeButton(fixture.nativeElement, 'alice').click();
    expect(confirm).toHaveBeenCalledWith(
      expect.objectContaining({ target: 'alice', consequence: 'You lose access to this team and its settings.' }),
    );
  });

  it('sends no removal when the confirmation is cancelled', () => {
    const { fixture, removeMember, confirm } = setup();
    confirm.mockImplementation(() => EMPTY);
    removeButton(fixture.nativeElement, 'bob').click();
    expect(removeMember).not.toHaveBeenCalled();
  });

  it('asks before revoking a pending invitation, naming the invited user', () => {
    const invitations = [{ user: 'carol', name: 'Carol', role: 'member', created_at: '2026-10-01T10:00:00', expires_at: '2026-10-08T10:00:00' }];
    const { fixture, confirm, revokeInvitation } = setup(undefined, false, undefined, invitations);
    const revoke = Array.from((fixture.nativeElement as HTMLElement).querySelectorAll<HTMLButtonElement>('button')).find(
      (b) => b.querySelector('.gr-button__label')?.textContent?.trim() === 'Revoke',
    )!;
    revoke.click();
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ title: 'Revoke invitation', target: 'carol' }));
    expect(revokeInvitation).toHaveBeenCalledWith('platform', 'carol');
  });
});

function removeButton(root: HTMLElement, user: string): HTMLButtonElement {
  const row = Array.from(root.querySelectorAll('gr-row')).find((r) => r.querySelector('.mono')?.textContent === user)!;
  return row.querySelector<HTMLButtonElement>('button.gr-button--danger')!;
}
