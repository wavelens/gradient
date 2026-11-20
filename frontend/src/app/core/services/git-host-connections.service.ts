/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable } from 'rxjs';
import { ApiService } from './api.service';
import {
  CreateGitHostConnectionRequest,
  GitHostConnectionOwner,
  GitHostConnectionResponse,
  GitHostConnectionSummary,
  GitHostConnectionTaskResponse,
  GitHostConnectionTestResponse,
  IncomingWebhookDeliveryResponse,
  PatchGitHostConnectionRequest,
} from '@core/models';

export function getOwnerGitHostConnectionsPathPrefix(owner: GitHostConnectionOwner): string {
  switch (owner.type) {
    case 'instance':
      return 'admin/git-host-connections';
    case 'team':
      return `teams/${encodeURIComponent(owner.name)}/git-host-connections`;
    case 'project':
      return `projects/${encodeURIComponent(owner.name)}/git-host-connections`;
  }
}

@Injectable({ providedIn: 'root' })
export class GitHostConnectionsApi {
  private api = inject(ApiService);

  list(owner: GitHostConnectionOwner, repository?: string): Observable<GitHostConnectionSummary[]> {
    const query = repository ? `?repository=${encodeURIComponent(repository)}` : '';
    return this.api.get<GitHostConnectionSummary[]>(`${getOwnerGitHostConnectionsPathPrefix(owner)}${query}`);
  }

  create(owner: GitHostConnectionOwner, body: CreateGitHostConnectionRequest): Observable<GitHostConnectionResponse> {
    return this.api.put<GitHostConnectionResponse>(getOwnerGitHostConnectionsPathPrefix(owner), body);
  }

  get(owner: GitHostConnectionOwner, id: string): Observable<GitHostConnectionResponse> {
    return this.api.get<GitHostConnectionResponse>(`${getOwnerGitHostConnectionsPathPrefix(owner)}/${id}`);
  }

  patch(owner: GitHostConnectionOwner, id: string, body: PatchGitHostConnectionRequest): Observable<GitHostConnectionResponse> {
    return this.api.patch<GitHostConnectionResponse>(`${getOwnerGitHostConnectionsPathPrefix(owner)}/${id}`, body);
  }

  delete(owner: GitHostConnectionOwner, id: string): Observable<boolean> {
    return this.api.delete<boolean>(`${getOwnerGitHostConnectionsPathPrefix(owner)}/${id}`);
  }

  listDeliveries(owner: GitHostConnectionOwner, id: string): Observable<IncomingWebhookDeliveryResponse[]> {
    return this.api.get<IncomingWebhookDeliveryResponse[]>(`${getOwnerGitHostConnectionsPathPrefix(owner)}/${id}/deliveries`);
  }

  listTasks(viewer: GitHostConnectionOwner, id: string): Observable<GitHostConnectionTaskResponse[]> {
    return this.api.get<GitHostConnectionTaskResponse[]>(`${getOwnerGitHostConnectionsPathPrefix(viewer)}/${id}/tasks`);
  }

  test(owner: GitHostConnectionOwner, id: string): Observable<GitHostConnectionTestResponse> {
    return this.api.post<GitHostConnectionTestResponse>(`${getOwnerGitHostConnectionsPathPrefix(owner)}/${id}/test`);
  }
}
