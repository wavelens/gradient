/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { of, throwError } from 'rxjs';
import { TeamMembersComponent } from './team-members.component';
import { TeamsService } from '@core/services/teams.service';
import { Team, TeamMember } from '@core/models';

const team: Team = {
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

const members: TeamMember[] = [
  { user: 'alice', name: 'Alice', role: 'admin', source: 'api' },
  { user: 'bob', name: 'Bob', role: 'member', source: 'group' },
  { user: 'dave', name: 'Dave', role: 'member', source: 'state' },
];

function setup(removeMember = vi.fn().mockReturnValue(of('User removed')), managed = false) {
  const addMember = vi.fn().mockReturnValue(of('Invitation sent'));
  TestBed.configureTestingModule({
    imports: [TeamMembersComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      { provide: ActivatedRoute, useValue: { snapshot: { paramMap: convertToParamMap({ team: 'platform' }) } } },
      {
        provide: TeamsService,
        useValue: {
          get: () => of({ ...team, managed }),
          members: () => of(members),
          invitations: () => of([]),
          addMember,
          updateMember: vi.fn().mockReturnValue(of('ok')),
          removeMember,
          revokeInvitation: vi.fn().mockReturnValue(of('ok')),
        },
      },
    ],
  });
  const fixture = TestBed.createComponent(TeamMembersComponent);
  fixture.detectChanges();
  return { fixture, addMember, removeMember };
}

describe('TeamMembersComponent', () => {
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
    addMember.mockReturnValue(throwError(() => new Error('User not found')));
    const component = fixture.componentInstance;
    component.openAdd();
    fixture.detectChanges();
    component.newMember = { user: 'nobody', role: 'member' };
    component.addMember();
    fixture.detectChanges();
    expect(document.querySelector('.gr-dialog')?.textContent).toContain('User not found');
    expect((fixture.nativeElement as HTMLElement).textContent).not.toContain('User not found');
  });

  it('shows why the last admin cannot be removed', () => {
    const conflict = vi.fn().mockReturnValue(
      throwError(() => new Error('Cannot remove the last Admin from the team.')),
    );
    const { fixture } = setup(conflict);
    fixture.componentInstance.removeMember('alice');
    fixture.detectChanges();
    expect((fixture.nativeElement as HTMLElement).textContent).toContain('last Admin');
  });
});
