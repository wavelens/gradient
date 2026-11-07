/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { of, throwError } from 'rxjs';
import { TeamMembersComponent } from './team-members.component';
import { TeamsService } from '@core/services/teams.service';
import { NotifyService } from '@core/services/notify.service';
import { ApiError } from '@core/services/api.service';
import { TeamResponse, TeamMemberItem } from '@core/models';

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

function setup(removeMember = vi.fn().mockReturnValue(of('User removed')), managed = false, listMembers = vi.fn().mockReturnValue(of(members))) {
  const addMember = vi.fn().mockReturnValue(of('Invitation sent'));
  const updateMember = vi.fn().mockReturnValue(of('ok'));
  const notify = { success: vi.fn(), failure: vi.fn() };
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
          invitations: () => of([]),
          addMember,
          updateMember,
          removeMember,
          revokeInvitation: vi.fn().mockReturnValue(of('ok')),
        },
      },
      { provide: NotifyService, useValue: notify },
    ],
  });
  const fixture = TestBed.createComponent(TeamMembersComponent);
  fixture.componentRef.setInput('team', { ...team, managed });
  fixture.detectChanges();
  return { fixture, addMember, updateMember, removeMember, notify };
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

  it('reports why the last admin cannot be removed', () => {
    const failure = new ApiError('Cannot remove the last Admin from the team.', 409);
    const { fixture, notify } = setup(vi.fn().mockReturnValue(throwError(() => failure)));
    fixture.componentInstance.removeMember('alice');
    expect(notify.failure).toHaveBeenCalledWith('Member removal failed', failure);
    expect(fixture.componentInstance.busy()).toBeNull();
  });

  it('reports a refused role change', () => {
    const { fixture, updateMember, notify } = setup();
    const failure = new ApiError('Forbidden', 403);
    updateMember.mockReturnValue(throwError(() => failure));
    fixture.componentInstance.updateRole('bob', 'admin');
    expect(notify.failure).toHaveBeenCalledWith('Role change failed', failure);
  });

  it('confirms a removed member', () => {
    const { fixture, notify } = setup();
    fixture.componentInstance.removeMember('bob');
    expect(notify.success).toHaveBeenCalledWith('Member removed');
  });
});
