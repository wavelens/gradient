/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { inject } from '@angular/core';
import { Observable, map } from 'rxjs';
import { ProjectsService } from '@core/services/projects.service';
import { CachesService } from '@core/services/caches.service';
import { TeamsService } from '@core/services/teams.service';
import type {
  CreateRoleRequest,
  PatchRoleRequest,
  PendingInvitationItem,
  PermissionEntry,
  RoleResponse_ProjectRoleOwner,
  TeamMemberItem,
  TeamRole,
} from '@core/models';
import type { MemberPlace } from '@shared/access';
import { type PeopleListRow, toPeopleListRowFromAccessPerson } from './access-reason-label';

export type ProjectOrCacheRole = Pick<RoleResponse_ProjectRoleOwner, 'id' | 'name' | 'builtin' | 'permissions'>;

export interface MembersPageRequests {
  people(): Observable<PeopleListRow[]>;
  invitations(): Observable<PendingInvitationItem[]>;
  addMember(user: string, role: string, addNow: boolean): Observable<string>;
  changeRole(user: string, role: string): Observable<string>;
  removeMember(user: string): Observable<string>;
  revokeInvitation(user: string): Observable<string>;
}

export interface ProjectOrCacheRoleRequests {
  roles(): Observable<{ roles: ProjectOrCacheRole[]; available_permissions: PermissionEntry[] }>;
  createRole(request: CreateRoleRequest): Observable<unknown>;
  updateRole(role: string, patch: PatchRoleRequest): Observable<unknown>;
  deleteRole(role: string): Observable<unknown>;
}

const teamMemberSourceLabels: Record<TeamMemberItem['source'], string> = {
  api: 'Direct',
  state: 'Direct · server configuration',
  group: 'From a group in the identity provider',
};

export function convertTeamMemberToPeopleListRow(member: TeamMemberItem): PeopleListRow {
  const managed = member.source === 'state';
  return {
    user: member.user,
    displayName: member.name,
    role: member.role,
    directRole: member.role,
    locked: managed,
    reasons: [{ kind: 'direct', role: member.role, applies: true, managed }],
    sources: [{ label: teamMemberSourceLabels[member.source], team: null }],
  };
}

export function buildMemberRequestForKindAndName(kind: MemberPlace, name: string): MembersPageRequests {
  if (kind === 'team') {
    const teams = inject(TeamsService);
    return {
      people: () => teams.members(name).pipe(map((members) => members.map(convertTeamMemberToPeopleListRow))),
      invitations: () => teams.invitations(name),
      addMember: (user, role, addNow) => teams.addMember(name, user, role as TeamRole, addNow),
      changeRole: (user, role) => teams.updateMember(name, user, role as TeamRole),
      removeMember: (user) => teams.removeMember(name, user),
      revokeInvitation: (user) => teams.revokeInvitation(name, user),
    };
  }
  if (kind === 'cache') {
    const caches = inject(CachesService);
    return {
      people: () => caches.getAccess(name).pipe(map((people) => people.map(toPeopleListRowFromAccessPerson))),
      invitations: () => caches.getInvitations(name),
      addMember: (user, role, addNow) => caches.addMember(name, user, role, addNow),
      changeRole: (user, role) => caches.updateMember(name, user, role),
      removeMember: (user) => caches.removeMember(name, user),
      revokeInvitation: (user) => caches.revokeInvitation(name, user),
    };
  }
  const projects = inject(ProjectsService);
  return {
    people: () => projects.getAccess(name).pipe(map((people) => people.map(toPeopleListRowFromAccessPerson))),
    invitations: () => projects.getInvitations(name),
    addMember: (user, role, addNow) => projects.addMember(name, user, role, addNow),
    changeRole: (user, role) => projects.updateMemberRole(name, user, role),
    removeMember: (user) => projects.removeMember(name, user),
    revokeInvitation: (user) => projects.revokeInvitation(name, user),
  };
}

export function buildRoleListCreateEditDeleteRequestsForKindAndName(kind: 'project' | 'cache', name: string): ProjectOrCacheRoleRequests {
  const service = kind === 'project' ? inject(ProjectsService) : inject(CachesService);
  return {
    roles: () => service.getRoles(name),
    createRole: (request) => service.createRole(name, request),
    updateRole: (role, patch) => service.updateRole(name, role, patch),
    deleteRole: (role) => service.deleteRole(name, role),
  };
}
