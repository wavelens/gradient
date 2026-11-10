/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { Observable, of } from 'rxjs';
import type { AccessState, PersonWithAccessToProjectOrCache, TeamMemberItem, TeamResponse } from '@core/models';
import { AuthService } from '@core/services/auth.service';
import { CachesService } from '@core/services/caches.service';
import { NotifyService } from '@core/services/notify.service';
import { ProjectsService } from '@core/services/projects.service';
import { TeamsService } from '@core/services/teams.service';
import { UserService } from '@core/services/user.service';
import { cacheAccessData, projectRouteStub } from '@core/resolvers/access-route.testing';
import { ConfirmRequest, ConfirmService } from '@shared/confirm';
import type { MembersPageTab } from '@shared/routing';
import { MembersPageComponent } from './members-page.component';

const admin: AccessState = { managed: false, canEdit: true, canTrigger: true };
const viewer: AccessState = { managed: false, canEdit: false, canTrigger: false };
const aliceTeam = { name: 'alice-team', display_name: 'Alice Team' };

const people: PersonWithAccessToProjectOrCache[] = [
  { user: 'alice', user_display_name: 'Alice', role: 'Admin', permissions: [], reasons: [{ kind: 'direct', role: 'Admin', applies: true, managed: false }] },
  {
    user: 'carol',
    user_display_name: 'Carol',
    role: 'Write',
    permissions: [],
    reasons: [
      { kind: 'direct', role: 'Write', applies: true, managed: false },
      { kind: 'team', team: aliceTeam, role: 'View', applies: false, managed: false },
    ],
  },
  { user: 'dave', user_display_name: 'Dave', role: 'View', permissions: [], reasons: [{ kind: 'owner', team: aliceTeam, role: 'View', applies: true, managed: false }] },
  { user: 'root', user_display_name: 'Root', role: null, permissions: [], reasons: [{ kind: 'superuser', applies: true, managed: false }] },
];

const builtInRoles = {
  roles: ['Admin', 'Write', 'View'].map((name) => ({ id: name, name, builtin: true, permissions: [], project: null })),
  available_permissions: [],
};

function memberService() {
  return {
    getAccess: vi.fn(() => of(people)),
    getInvitations: vi.fn(() => of([])),
    getRoles: vi.fn(() => of(builtInRoles)),
    addMember: vi.fn(() => of('ok')),
    updateMemberRole: vi.fn(() => of('ok')),
    updateMember: vi.fn(() => of('ok')),
    removeMember: vi.fn(() => of('ok')),
    getProject: vi.fn(() => of({})),
    getCache: vi.fn(() => of({})),
  };
}

function setup(route: Partial<ActivatedRoute>, options: { superuser?: boolean; teamMembers?: TeamMemberItem[] } = {}) {
  const projects = memberService();
  const caches = memberService();
  const teams = {
    members: vi.fn(() => of(options.teamMembers ?? [])),
    invitations: vi.fn(() => of([])),
    projectGrants: () => of([]),
    cacheGrants: () => of([]),
    get: () => of({}),
  };
  const notify = { success: vi.fn(), failure: vi.fn() };
  const confirm = vi.fn((request: ConfirmRequest): Observable<unknown> => request.action());
  TestBed.configureTestingModule({
    imports: [MembersPageComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      { provide: ActivatedRoute, useValue: route },
      { provide: AuthService, useValue: { user: signal({ username: 'alice', superuser: options.superuser ?? false }), isAuthenticated: () => true } },
      { provide: ProjectsService, useValue: projects },
      { provide: CachesService, useValue: caches },
      { provide: TeamsService, useValue: teams },
      { provide: UserService, useValue: { searchUsers: () => of([]) } },
      { provide: NotifyService, useValue: notify },
      { provide: ConfirmService, useValue: { confirm } },
    ],
  });
  const fixture = TestBed.createComponent(MembersPageComponent);
  fixture.detectChanges();
  return { fixture, root: fixture.nativeElement as HTMLElement, projects, caches, teams, notify, confirm };
}

function projectRoute(access: AccessState, tab: MembersPageTab = 'people', role?: string | null): Partial<ActivatedRoute> {
  const route = projectRouteStub('acme', access, role === undefined ? {} : { role, permissions: [] });
  (route.snapshot as unknown as { data: Record<string, unknown> }).data['membersPageTab'] = tab;
  return route;
}

function cacheRoute(access: AccessState, role: string | null): Partial<ActivatedRoute> {
  return {
    snapshot: { paramMap: convertToParamMap({ cache: 'main' }), data: { membersPageTab: 'people' } } as never,
    data: of({}),
    parent: { data: of(cacheAccessData(access, { name: 'main', role })) } as never,
  };
}

function teamRoute(role: TeamResponse['role']): Partial<ActivatedRoute> {
  const data = { teamAccess: { team: { name: 'platform', display_name: 'Platform', managed: false, role } }, membersPageTab: 'people' };
  return { snapshot: { paramMap: convertToParamMap({ team: 'platform' }), data } as never, data: of(data) };
}

function tabLabels(root: HTMLElement): string[] {
  return Array.from(root.querySelectorAll('.link-tab'), (tab) => tab.textContent?.trim() ?? '');
}

function rowMeta(root: HTMLElement, user: string): string {
  const row = Array.from(root.querySelectorAll('gr-row')).find((entry) => entry.querySelector('.entity-name__slug')?.textContent?.trim() === user);
  return row?.querySelector('[slot="meta"]')?.textContent?.replace(/\s+/g, ' ').trim() ?? '';
}

describe('MembersPageComponent tabs', () => {
  it('shows Pending only to people who manage members and Roles only to members', () => {
    expect(tabLabels(setup(projectRoute(admin)).root)).toEqual(['People', 'Teams', 'Pending', 'Roles']);
    TestBed.resetTestingModule();
    expect(tabLabels(setup(projectRoute(viewer)).root)).toEqual(['People', 'Teams', 'Roles']);
    TestBed.resetTestingModule();
    expect(tabLabels(setup(projectRoute(viewer, 'people', null)).root)).toEqual(['People', 'Teams']);
  });

  it('links each tab to its own address', () => {
    const hrefs = Array.from(setup(projectRoute(admin)).root.querySelectorAll('.link-tab'), (tab) => tab.getAttribute('href'));
    expect(hrefs).toEqual(['/project/acme/members/people', '/project/acme/members/teams', '/project/acme/members/pending', '/project/acme/members/roles']);
  });

  it('blocks the Pending tab for people who cannot manage members', () => {
    const { root, projects } = setup(projectRoute(viewer, 'pending'));
    expect(root.querySelector('app-pending-invitations-and-team-grant-requests')).toBeNull();
    expect(projects.getInvitations).not.toHaveBeenCalled();
  });
});

describe('MembersPageComponent people', () => {
  it('names where the access of every person comes from', () => {
    const { root } = setup(projectRoute(admin));
    expect(rowMeta(root, 'carol')).toBe('Direct · From Alice Team (alice-team), overridden by direct role');
    expect(rowMeta(root, 'dave')).toBe('From owner team Alice Team (alice-team)');
    expect(rowMeta(root, 'root')).toBe('Instance administrator');
  });

  it('shows the display name of every person with the username beside it', () => {
    const name = setup(projectRoute(admin)).root.querySelector('gr-row gr-entity-name');
    expect(name?.querySelector('.entity-name__display')?.textContent?.trim()).toBe('Alice');
    expect(name?.querySelector('.entity-name__slug')?.textContent?.trim()).toBe('alice');
    expect(name?.getAttribute('title')).toBe('Alice (alice)');
  });

  it('shows roles as read-only badges without Invite Member to a viewer', () => {
    const { root } = setup(projectRoute(viewer));
    expect(root.querySelector('gr-row gr-select')).toBeNull();
    expect(root.querySelector('gr-row gr-badge')?.textContent?.trim()).toBe('Admin');
    expect(root.textContent).not.toContain('Invite Member');
  });

  it('changes a direct role in place', () => {
    const { fixture, projects, notify } = setup(projectRoute(admin));
    fixture.componentInstance.updateMemberRole({ user: 'carol', role: 'Admin' });
    expect(projects.updateMemberRole).toHaveBeenCalledWith('acme', 'carol', 'Admin');
    expect(projects.addMember).not.toHaveBeenCalled();
    expect(notify.success).toHaveBeenCalledWith('Member role saved');
  });

  it('adds a direct role right away for someone who has access only through a team and reloads the sources', () => {
    const { fixture, projects } = setup(projectRoute(admin));
    fixture.componentInstance.updateMemberRole({ user: 'dave', role: 'Write' });
    expect(projects.addMember).toHaveBeenCalledWith('acme', 'dave', 'Write', true);
    expect(projects.getAccess).toHaveBeenCalledTimes(2);
  });

  it('says which team access remains on removal and links to that team', () => {
    const { fixture, projects, confirm, notify } = setup(projectRoute(admin));
    fixture.componentInstance.removeMember('carol');
    const request = confirm.mock.calls[0][0];
    expect(request.consequence).toBe('Roles for carol still include View through Alice Team (alice-team).');
    expect(request.consequenceLink?.label).toBe('Open Alice Team (alice-team)');
    expect(String(request.consequenceLink?.link)).toBe('/team/alice-team');
    expect(projects.removeMember).toHaveBeenCalledWith('acme', 'carol');
    expect(notify.success).toHaveBeenCalledWith('Member deleted');
  });

  it('removes a person without team access from the list and links nowhere', () => {
    const { fixture, confirm } = setup(projectRoute(admin));
    fixture.componentInstance.removeMember('alice');
    expect(confirm.mock.calls[0][0].consequenceLink).toBeUndefined();
    expect(confirm.mock.calls[0][0].consequence).toBe('You lose all access to this project.');
    expect(fixture.componentInstance.rows().map((row) => row.user)).not.toContain('alice');
  });
});

describe('MembersPageComponent other places', () => {
  it('shows no access on a cache to people outside it without asking the server', () => {
    const { root, caches } = setup(cacheRoute(viewer, null));
    expect(caches.getAccess).not.toHaveBeenCalled();
    expect(root.textContent).toContain('Only the members of this cache can view this.');
  });

  it('lists the people of a cache to its members', () => {
    const { root, caches } = setup(cacheRoute(viewer, 'View'));
    expect(caches.getAccess).toHaveBeenCalledWith('main');
    expect(rowMeta(root, 'dave')).toBe('From owner team Alice Team (alice-team)');
  });

  it('lists team members with their source and offers People and Pending to team admins', () => {
    const { root, teams } = setup(teamRoute('admin'), {
      teamMembers: [
        { user: 'alice', name: 'Alice', role: 'admin', source: 'api' },
        { user: 'erin', name: 'Erin', role: 'member', source: 'group' },
      ],
    });
    expect(teams.members).toHaveBeenCalledWith('platform');
    expect(tabLabels(root)).toEqual(['People', 'Pending']);
    expect(rowMeta(root, 'erin')).toBe('From a group in the identity provider');
  });
});
