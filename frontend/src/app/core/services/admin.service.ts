/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable, of } from 'rxjs';
import { map, catchError } from 'rxjs/operators';
import { ApiService } from '@core/services/api.service';

export interface GithubAppManifestResponse {
  manifest: Record<string, unknown>;
  post_url: string;
  state: string;
}

export interface GithubAppCredentials {
  id: number;
  slug: string;
  html_url: string;
  pem: string;
  webhook_secret: string;
  client_id: string;
  client_secret: string;
}

export interface AdminTask {
  id: string;
  kind: string;
  status: string;
  created_at: string;
  started_at: string | null;
  finished_at: string | null;
  progress: unknown | null;
  error: string | null;
  created_by: string | null;
}

export interface StartDeepGcResponse {
  task_id: string;
  status: string;
}

@Injectable({ providedIn: 'root' })
export class AdminService {
  private api = inject(ApiService);

  requestGithubAppManifest(host?: string): Observable<GithubAppManifestResponse> {
    return this.api.post<GithubAppManifestResponse>('admin/github-app/manifest', { host });
  }

  fetchGithubAppCredentials(): Observable<GithubAppCredentials> {
    return this.api.get<GithubAppCredentials>('admin/github-app/credentials');
  }

  startDeepGc(): Observable<StartDeepGcResponse> {
    return this.api.post<StartDeepGcResponse>('admin/maintenance/deep-gc', {});
  }

  listTasks(): Observable<AdminTask[]> {
    return this.api.get<AdminTask[]>('admin/tasks');
  }

  githubAppConfigured(): Observable<boolean> {
    return this.fetchGithubAppCredentials().pipe(map(() => true), catchError(() => of(false)));
  }
}
