/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable } from 'rxjs';
import { ApiService } from './api.service';
import {
  AllowedCapabilities,
  ProjectWorkerEntry,
  WorkerMetricsResponse,
  RegisterWorkerResponse,
  PatchWorkerRequest,
  WorkerTestResponse,
  ConnectionRequest,
  ConnectionResponse,
} from '@core/models';

@Injectable({ providedIn: 'root' })
export class WorkersService {
  private api = inject(ApiService);

  getWorkers(project: string): Observable<ProjectWorkerEntry[]> {
    return this.api.get<ProjectWorkerEntry[]>(`projects/${project}/workers`);
  }

  getWorkerMetrics(project: string, workerId: string): Observable<WorkerMetricsResponse> {
    return this.api.get<WorkerMetricsResponse>(`projects/${project}/workers/${workerId}/metrics`);
  }

  registerWorker(
    project: string,
    workerId: string,
    displayName: string,
    url?: string,
    token?: string,
    caps?: AllowedCapabilities,
  ): Observable<RegisterWorkerResponse> {
    return this.api.post<RegisterWorkerResponse>(`projects/${project}/workers`, {
      worker_id: workerId,
      display_name: displayName,
      url: url || undefined,
      token: token || undefined,
      enable_network: caps?.enable_network ?? true,
      enable_secrets: caps?.enable_secrets ?? true,
      enable_exec: caps?.enable_exec ?? false,
      enable_eval: caps?.enable_eval ?? true,
      enable_build: caps?.enable_build ?? true,
    });
  }

  setWorkerActive(project: string, workerId: string, active: boolean): Observable<string> {
    return this.api.patch<string>(`projects/${project}/workers/${workerId}`, { active });
  }

  patchWorker(project: string, workerId: string, body: PatchWorkerRequest): Observable<string> {
    return this.api.patch<string>(`projects/${project}/workers/${workerId}`, body);
  }

  deleteWorker(project: string, workerId: string): Observable<string> {
    return this.api.delete<string>(`projects/${project}/workers/${workerId}`);
  }

  testWorker(project: string, workerId: string): Observable<WorkerTestResponse> {
    return this.api.post<WorkerTestResponse>(`projects/${project}/workers/${workerId}/test`, {});
  }

  connectGradientCi(body: ConnectionRequest): Observable<ConnectionResponse> {
    return this.api.post<ConnectionResponse>('gradient-ci/connections', body);
  }
}
