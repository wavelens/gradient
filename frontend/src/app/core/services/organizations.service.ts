/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable } from 'rxjs';
import { ApiService } from './api.service';
import { Organization } from '@core/models';

export interface OrgMember {
  id: string;   // username
  name: string; // role name (e.g., "Admin")
}

@Injectable({ providedIn: 'root' })
export class OrganizationsService {
  private api = inject(ApiService);

  getOrganizations(): Observable<Organization[]> {
    return this.api.get<Organization[]>('orgs');
  }

  getOrganization(name: string): Observable<Organization> {
    return this.api.get<Organization>(`orgs/${name}`);
  }

  createOrganization(data: {
    name: string;
    display_name: string;
    description: string;
  }): Observable<string> {
    return this.api.put<string>('orgs', data);
  }

  updateOrganization(
    name: string,
    data: Partial<Organization>
  ): Observable<string> {
    return this.api.patch<string>(`orgs/${name}`, data);
  }

  deleteOrganization(name: string): Observable<string> {
    return this.api.delete<string>(`orgs/${name}`);
  }

  getMembers(org: string): Observable<OrgMember[]> {
    return this.api.get<OrgMember[]>(`orgs/${org}/users`);
  }

  addMember(org: string, user: string, role: string): Observable<string> {
    return this.api.post<string>(`orgs/${org}/users`, { user, role });
  }

  updateMemberRole(org: string, user: string, role: string): Observable<string> {
    return this.api.patch<string>(`orgs/${org}/users`, { user, role });
  }

  removeMember(org: string, user: string): Observable<string> {
    return this.api.delete<string>(`orgs/${org}/users`, { user });
  }

  getSSHKey(org: string): Observable<string> {
    return this.api.get<string>(`orgs/${org}/ssh`);
  }

  generateSSHKey(org: string): Observable<string> {
    return this.api.post<string>(`orgs/${org}/ssh`);
  }
}
