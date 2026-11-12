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
  RoleListResponse_ProjectRoleOwner,
  RoleResponse_ProjectRoleOwner,
  ProjectSummary,
  PendingInvitationItem,
  CacheSubscriptionItem,
  CacheSubscriptionMode,
  SubscribableCache,
  ProjectResponse,
  MakeProjectRequest,
  GitSshHostKeyResponse,
  SaveHostKeyRequestBody,
  ScannedHostKey,
  PatchProjectRequest,
  CreateRoleRequest,
  PatchRoleRequest,
  PublicProjectSummary,
  CreatedEntityResponse,
  PersonWithAccessToProjectOrCache,
  AddMemberRequest,
  ProjectReadinessResponse,
} from '@core/models';
import { asSectionRequest } from '@core/interceptors/request-context';

@Injectable({ providedIn: 'root' })
export class ProjectsService {
  private api = inject(ApiService);

  getProjects(page = 1, perPage = 50): Observable<Paginated<ProjectSummary[]>> {
    return this.api.get<Paginated<ProjectSummary[]>>(`projects?page=${page}&per_page=${perPage}`);
  }

  getPublicProjects(page = 1, perPage = 50): Observable<Paginated<PublicProjectSummary[]>> {
    return this.api.get<Paginated<PublicProjectSummary[]>>(`projects/public?page=${page}&per_page=${perPage}`);
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

  createProject(data: MakeProjectRequest): Observable<CreatedEntityResponse> {
    return this.api.put<CreatedEntityResponse>('projects', data);
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

  getAccess(project: string): Observable<PersonWithAccessToProjectOrCache[]> {
    return this.api.get<PersonWithAccessToProjectOrCache[]>(`projects/${project}/access`);
  }

  addMember(project: string, user: string, role: string, addNow: boolean): Observable<string> {
    const request: AddMemberRequest = { user, role, add_as_member_immediately: addNow };
    return this.api.post<string>(`projects/${project}/users`, request);
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

  listSSHHostKeys(project: string): Observable<GitSshHostKeyResponse[]> {
    return this.api.get<GitSshHostKeyResponse[]>(`projects/${project}/ssh/host-keys`);
  }

  scanSavedHostKey(project: string, id: string): Observable<ScannedHostKey> {
    return this.api.post<ScannedHostKey>(`projects/${project}/ssh/host-keys/${id}/scan`);
  }

  saveHostKey(project: string, body: SaveHostKeyRequestBody): Observable<GitSshHostKeyResponse> {
    return this.api.put<GitSshHostKeyResponse>(`projects/${project}/ssh/host-keys`, body);
  }

  deleteSavedHostKey(project: string, id: string): Observable<boolean> {
    return this.api.delete<boolean>(`projects/${project}/ssh/host-keys/${id}`);
  }

  checkProjectNameAvailable(name: string): Observable<boolean> {
    return this.api.get<boolean>(`projects/available?name=${encodeURIComponent(name)}`);
  }

  getProjectReadiness(project: string): Observable<ProjectReadinessResponse> {
    return this.api.get<ProjectReadinessResponse>(`projects/${project}/readiness`, asSectionRequest());
  }

  getSubscribedCaches(project: string): Observable<CacheSubscriptionItem[]> {
    return this.api.get<CacheSubscriptionItem[]>(`projects/${project}/subscribe`);
  }

  getSubscribableCaches(project: string): Observable<SubscribableCache[]> {
    return this.api.get<SubscribableCache[]>(`projects/${project}/subscribable-caches`);
  }

  subscribeCache(project: string, cache: string, mode?: CacheSubscriptionMode): Observable<string> {
    return this.api.post<string>(`projects/${project}/subscribe/${cache}`, mode ? { mode } : {});
  }

  setCacheSubscriptionSignOutputs(project: string, cache: string, signOutputs: boolean): Observable<string> {
    return this.api.patch<string>(`projects/${project}/subscribe/${cache}`, { sign_outputs: signOutputs });
  }

  unsubscribeCache(project: string, cache: string): Observable<string> {
    return this.api.delete<string>(`projects/${project}/subscribe/${cache}`);
  }
}
