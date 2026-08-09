/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable } from 'rxjs';
import { ApiService } from './api.service';
import { Project, Paginated } from '@core/models';
import { PermissionDescriptor } from '@core/models/permission.model';

export interface ProjectMember {
  id: string;   // username
  name: string; // role name (e.g., "Admin")
}

export interface ProjectRole {
  id: string;
  name: string;
  project: string | null;
  builtin: boolean;
  permissions: string[];
}

export type { PermissionDescriptor };

export interface RoleListResponse {
  roles: ProjectRole[];
  available_permissions: PermissionDescriptor[];
}

@Injectable({ providedIn: 'root' })
export class ProjectsService {
  private api = inject(ApiService);

  getProjects(page = 1, perPage = 50): Observable<Paginated<Project[]>> {
    return this.api.get<Paginated<Project[]>>(`projects?page=${page}&per_page=${perPage}`);
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

  getProject(name: string): Observable<Project> {
    return this.api.get<Project>(`projects/${name}`);
  }

  createProject(data: {
    name: string;
    display_name: string;
    description: string;
    public?: boolean;
  }): Observable<string> {
    return this.api.put<string>('projects', data);
  }

  updateProject(
    name: string,
    data: Partial<Project>
  ): Observable<string> {
    return this.api.patch<string>(`projects/${name}`, data);
  }

  deleteProject(name: string): Observable<string> {
    return this.api.delete<string>(`projects/${name}`);
  }

  getMembers(project: string): Observable<ProjectMember[]> {
    return this.api.get<ProjectMember[]>(`projects/${project}/users`);
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

  getRoles(project: string): Observable<RoleListResponse> {
    return this.api.get<RoleListResponse>(`projects/${project}/roles`);
  }

  createRole(
    project: string,
    data: { name: string; permissions: string[] }
  ): Observable<ProjectRole> {
    return this.api.post<ProjectRole>(`projects/${project}/roles`, data);
  }

  updateRole(
    project: string,
    roleId: string,
    data: { name?: string; permissions?: string[] }
  ): Observable<ProjectRole> {
    return this.api.patch<ProjectRole>(`projects/${project}/roles/${roleId}`, data);
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

  getSubscribedCaches(project: string): Observable<{ id: string; name: string }[]> {
    return this.api.get<{ id: string; name: string }[]>(`projects/${project}/subscribe`);
  }

  subscribeCache(project: string, cache: string): Observable<string> {
    return this.api.post<string>(`projects/${project}/subscribe/${cache}`, {});
  }

  unsubscribeCache(project: string, cache: string): Observable<string> {
    return this.api.delete<string>(`projects/${project}/subscribe/${cache}`);
  }
}
