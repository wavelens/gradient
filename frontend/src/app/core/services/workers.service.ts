/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable } from 'rxjs';
import { ApiService } from './api.service';
import {
  ProjectWorkerEntry,
  WorkerMetricsResponse,
  RegisterWorkerRequest,
  WorkerRegistrationResponse,
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

  registerWorker(project: string, worker: RegisterWorkerRequest): Observable<WorkerRegistrationResponse> {
    return this.api.post<WorkerRegistrationResponse>(`projects/${project}/workers`, worker);
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
