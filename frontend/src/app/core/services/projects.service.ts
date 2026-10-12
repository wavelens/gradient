/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable } from 'rxjs';
import { ApiService } from './api.service';
import {
  Paginated,
  MemberItem,
  RoleListResponse_ProjectRoleOwner,
  RoleResponse_ProjectRoleOwner,
  ProjectSummary,
  PendingInvitationItem,
  CacheSubscriptionItem,
  ProjectResponse,
  MakeProjectRequest,
  PatchProjectRequest,
  CreateRoleRequest,
  PatchRoleRequest,
  Project,
} from '@core/models';

@Injectable({ providedIn: 'root' })
export class ProjectsService {
  private api = inject(ApiService);

  getProjects(page = 1, perPage = 50): Observable<Paginated<ProjectSummary[]>> {
    return this.api.get<Paginated<ProjectSummary[]>>(`projects?page=${page}&per_page=${perPage}`);
  }

  getPublicProjects(page = 1, perPage = 50): Observable<Paginated<Project[]>> {
    return this.api.get<Paginated<Project[]>>(`projects/public?page=${page}&per_page=${perPage}`);
  }

  setPublic(name: string): Observable<string> {
    return this.api.post<string>(`projects/${name}/public`);
  }

  setPrivate(name: string): Observable<string> {
    return this.api.delete<string>(`projects/${name}/public`);
  }

  getProject(name: string): Observable<ProjectResponse> {
    return this.api.get<ProjectResponse>(`projects/${name}`);
  }

  createProject(data: MakeProjectRequest): Observable<string> {
    return this.api.put<string>('projects', data);
  }

  updateProject(
    name: string,
    data: PatchProjectRequest
  ): Observable<string> {
    return this.api.patch<string>(`projects/${name}`, data);
  }

  deleteProject(name: string): Observable<string> {
    return this.api.delete<string>(`projects/${name}`);
  }

  getMembers(project: string): Observable<MemberItem[]> {
    return this.api.get<MemberItem[]>(`projects/${project}/users`);
  }

  addMember(project: string, user: string, role: string): Observable<string> {
    return this.api.post<string>(`projects/${project}/users`, { user, role });
  }

  updateMemberRole(project: string, user: string, role: string): Observable<string> {
    return this.api.patch<string>(`projects/${project}/users`, { user, role });
  }

  removeMember(project: string, user: string): Observable<string> {
    return this.api.delete<string>(`projects/${project}/users`, { user });
  }

  getInvitations(project: string): Observable<PendingInvitationItem[]> {
    return this.api.get<PendingInvitationItem[]>(`projects/${project}/invitations`);
  }

  revokeInvitation(project: string, user: string): Observable<string> {
    return this.api.delete<string>(`projects/${project}/invitations`, { user });
  }

  getRoles(project: string): Observable<RoleListResponse_ProjectRoleOwner> {
    return this.api.get<RoleListResponse_ProjectRoleOwner>(`projects/${project}/roles`);
  }

  createRole(
    project: string,
    data: CreateRoleRequest
  ): Observable<RoleResponse_ProjectRoleOwner> {
    return this.api.post<RoleResponse_ProjectRoleOwner>(`projects/${project}/roles`, data);
  }

  updateRole(
    project: string,
    roleId: string,
    data: PatchRoleRequest
  ): Observable<RoleResponse_ProjectRoleOwner> {
    return this.api.patch<RoleResponse_ProjectRoleOwner>(`projects/${project}/roles/${roleId}`, data);
  }

  deleteRole(project: string, roleId: string): Observable<boolean> {
    return this.api.delete<boolean>(`projects/${project}/roles/${roleId}`);
  }

  getSSHKey(project: string): Observable<string> {
    return this.api.get<string>(`projects/${project}/ssh`);
  }

  generateSSHKey(project: string): Observable<string> {
    return this.api.post<string>(`projects/${project}/ssh`);
  }

  checkProjectNameAvailable(name: string): Observable<boolean> {
    return this.api.get<boolean>(`projects/available?name=${encodeURIComponent(name)}`);
  }

  getSubscribedCaches(project: string): Observable<CacheSubscriptionItem[]> {
    return this.api.get<CacheSubscriptionItem[]>(`projects/${project}/subscribe`);
  }

  subscribeCache(project: string, cache: string): Observable<string> {
    return this.api.post<string>(`projects/${project}/subscribe/${cache}`, {});
  }

  unsubscribeCache(project: string, cache: string): Observable<string> {
    return this.api.delete<string>(`projects/${project}/subscribe/${cache}`);
  }
}
