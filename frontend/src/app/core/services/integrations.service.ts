/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable } from 'rxjs';
import { ApiService } from './api.service';
import {
  IntegrationResponse,
  IntegrationSummaryResponse,
  CreateIntegrationRequest,
  PatchIntegrationRequest,
} from '@core/models';

@Injectable({ providedIn: 'root' })
export class IntegrationsService {
  private api = inject(ApiService);

  listProjectIntegrations(project: string): Observable<IntegrationResponse[]> {
    return this.api.get<IntegrationResponse[]>(`projects/${project}/integrations`);
  }

  /** Credential-free integration list available to any project member.
   *  Use this for UIs that only need name/git_host_type - e.g. populating the
   *  trigger create/edit dropdown - instead of the admin-gated full list. */
  listProjectIntegrationSummaries(project: string): Observable<IntegrationSummaryResponse[]> {
    return this.api.get<IntegrationSummaryResponse[]>(`projects/${project}/integrations/summary`);
  }

  createProjectIntegration(project: string, body: CreateIntegrationRequest): Observable<IntegrationResponse> {
    return this.api.put<IntegrationResponse>(`projects/${project}/integrations`, body);
  }

  getProjectIntegration(project: string, id: string): Observable<IntegrationResponse> {
    return this.api.get<IntegrationResponse>(`projects/${project}/integrations/${id}`);
  }

  patchProjectIntegration(project: string, id: string, body: PatchIntegrationRequest): Observable<IntegrationResponse> {
    return this.api.patch<IntegrationResponse>(`projects/${project}/integrations/${id}`, body);
  }

  deleteProjectIntegration(project: string, id: string): Observable<boolean> {
    return this.api.delete<boolean>(`projects/${project}/integrations/${id}`);
  }
}
