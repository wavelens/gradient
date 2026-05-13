/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable } from 'rxjs';
import { ApiService } from './api.service';
import {
  CreateIntegrationRequest,
  Integration,
  IntegrationSummary,
  PatchIntegrationRequest,
  ProjectIntegrationLink,
  SetProjectIntegrationRequest,
} from '@core/models';

@Injectable({ providedIn: 'root' })
export class IntegrationsService {
  private api = inject(ApiService);

  listOrgIntegrations(org: string): Observable<Integration[]> {
    return this.api.get<Integration[]>(`orgs/${org}/integrations`);
  }

  /** Credential-free integration list available to any org member.
   *  Use this for UIs that only need name/forge_type — e.g. populating the
   *  trigger create/edit dropdown — instead of the admin-gated full list. */
  listOrgIntegrationSummaries(org: string): Observable<IntegrationSummary[]> {
    return this.api.get<IntegrationSummary[]>(`orgs/${org}/integrations/summary`);
  }

  createOrgIntegration(org: string, body: CreateIntegrationRequest): Observable<Integration> {
    return this.api.put<Integration>(`orgs/${org}/integrations`, body);
  }

  getOrgIntegration(org: string, id: string): Observable<Integration> {
    return this.api.get<Integration>(`orgs/${org}/integrations/${id}`);
  }

  patchOrgIntegration(org: string, id: string, body: PatchIntegrationRequest): Observable<Integration> {
    return this.api.patch<Integration>(`orgs/${org}/integrations/${id}`, body);
  }

  deleteOrgIntegration(org: string, id: string): Observable<boolean> {
    return this.api.delete<boolean>(`orgs/${org}/integrations/${id}`);
  }

  getProjectIntegration(org: string, project: string): Observable<ProjectIntegrationLink> {
    return this.api.get<ProjectIntegrationLink>(`projects/${org}/${project}/integration`);
  }

  setProjectIntegration(
    org: string,
    project: string,
    body: SetProjectIntegrationRequest,
  ): Observable<ProjectIntegrationLink> {
    return this.api.put<ProjectIntegrationLink>(`projects/${org}/${project}/integration`, body);
  }

  deleteProjectIntegration(org: string, project: string): Observable<boolean> {
    return this.api.delete<boolean>(`projects/${org}/${project}/integration`);
  }
}
