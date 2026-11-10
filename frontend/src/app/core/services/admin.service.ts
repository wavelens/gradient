/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable } from 'rxjs';
import { ApiService } from '@core/services/api.service';
import {
  AdminStorageResponse,
  AdminWorkerListEntry,
  DrainingResponse,
  EffectiveConfigOption,
  GithubAppManifestResponse,
  ManifestResult,
  StartDeepGcResponse,
  AdminTaskDto,
  RevokedUserAccessCounts,
  UserAccessSummary,
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

  getUserAccessToAllProjectsCachesAndTeams(user: string): Observable<UserAccessSummary> {
    return this.api.get<UserAccessSummary>(`users/${encodeURIComponent(user)}/access`);
  }

  removeUserMembershipsAndInvitationsAndReturnCounts(user: string): Observable<RevokedUserAccessCounts> {
    return this.api.post<RevokedUserAccessCounts>(`users/${encodeURIComponent(user)}/revoke-access`, {});
  }

  listWorkers(): Observable<AdminWorkerListEntry[]> {
    return this.api.get<AdminWorkerListEntry[]>('admin/workers');
  }

  setWorkerDraining(worker: string, draining: boolean): Observable<AdminWorkerListEntry> {
    return this.api.patch<AdminWorkerListEntry>(`admin/workers/${encodeURIComponent(worker)}`, { draining });
  }

  getDiskAndStorageUsageByCacheProjectAndTask(): Observable<AdminStorageResponse> {
    return this.api.get<AdminStorageResponse>('admin/storage');
  }

  startGcRound(): Observable<void> {
    return this.api.post<void>('admin/maintenance/gc', {});
  }

  getServerOptionsWithValuesAndSources(): Observable<EffectiveConfigOption[]> {
    return this.api.get<EffectiveConfigOption[]>('admin/config');
  }
}
