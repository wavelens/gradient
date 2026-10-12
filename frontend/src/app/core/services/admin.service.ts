/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable } from 'rxjs';
import { ApiService } from '@core/services/api.service';
import {
  DrainingResponse,
  GithubAppManifestResponse,
  ManifestResult,
  StartDeepGcResponse,
  AdminTaskDto,
} from '@core/models';

@Injectable({ providedIn: 'root' })
export class AdminService {
  private api = inject(ApiService);

  requestGithubAppManifest(host?: string): Observable<GithubAppManifestResponse> {
    return this.api.post<GithubAppManifestResponse>('admin/github-app/manifest', { host });
  }

  fetchGithubAppCredentials(): Observable<ManifestResult> {
    return this.api.get<ManifestResult>('admin/github-app/credentials');
  }

  startDeepGc(): Observable<StartDeepGcResponse> {
    return this.api.post<StartDeepGcResponse>('admin/maintenance/deep-gc', {});
  }

  setDraining(enabled: boolean): Observable<DrainingResponse> {
    return this.api.post<DrainingResponse>('admin/draining', { enabled });
  }

  listTasks(): Observable<AdminTaskDto[]> {
    return this.api.get<AdminTaskDto[]>('admin/tasks');
  }
}
