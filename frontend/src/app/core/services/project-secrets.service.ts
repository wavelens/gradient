/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable } from 'rxjs';
import { ApiService } from './api.service';
import { ProjectSecretSummary, PutProjectSecretRequest, SopsRecipientResponse } from '@core/models';

@Injectable({ providedIn: 'root' })
export class ProjectSecretsService {
  private api = inject(ApiService);

  list(project: string): Observable<ProjectSecretSummary[]> {
    return this.api.get<ProjectSecretSummary[]>(`projects/${project}/secrets`);
  }

  put(project: string, name: string, value: string): Observable<ProjectSecretSummary> {
    const body: PutProjectSecretRequest = { value };
    return this.api.put<ProjectSecretSummary>(`projects/${project}/secrets/${name}`, body);
  }

  remove(project: string, name: string): Observable<void> {
    return this.api.delete<void>(`projects/${project}/secrets/${name}`);
  }

  sopsRecipient(project: string): Observable<SopsRecipientResponse> {
    return this.api.get<SopsRecipientResponse>(`projects/${project}/sops-recipient`);
  }
}
