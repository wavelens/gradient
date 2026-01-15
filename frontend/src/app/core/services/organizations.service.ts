/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable } from 'rxjs';
import { ApiService } from './api.service';
import { Organization } from '@core/models';

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
  }): Observable<Organization> {
    return this.api.put<Organization>('orgs', data);
  }

  updateOrganization(
    name: string,
    data: Partial<Organization>
  ): Observable<Organization> {
    return this.api.patch<Organization>(`orgs/${name}`, data);
  }

  deleteOrganization(name: string): Observable<void> {
    return this.api.delete<void>(`orgs/${name}`);
  }
}
