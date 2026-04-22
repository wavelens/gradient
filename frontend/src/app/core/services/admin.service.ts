/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable } from 'rxjs';
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

@Injectable({ providedIn: 'root' })
export class AdminService {
  private api = inject(ApiService);

  requestGithubAppManifest(host?: string): Observable<GithubAppManifestResponse> {
    return this.api.post<GithubAppManifestResponse>('admin/github-app/manifest', { host });
  }

  fetchGithubAppCredentials(): Observable<GithubAppCredentials> {
    return this.api.get<GithubAppCredentials>('admin/github-app/credentials');
  }
}
