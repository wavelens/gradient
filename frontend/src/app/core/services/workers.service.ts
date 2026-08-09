/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable } from 'rxjs';
import { ApiService } from './api.service';
import { Worker, WorkerRegistration, WorkerTestResponse } from '@core/models';

export interface WorkerSamplePoint {
  at: string;
  cpu_usage_pct: number | null;
  ram_free_mb: number | null;
  ram_total_mb: number | null;
  disk_speed_mbps: number | null;
  network_speed_mbps: number | null;
  assigned_jobs: number;
  max_concurrent_builds: number;
  state: number;
}

export interface WorkerConnectionEntry {
  connected_at: string;
  disconnected_at: string | null;
}

export interface WorkerMetricsResponse {
  samples: WorkerSamplePoint[];
  connections: WorkerConnectionEntry[];
  jobs_dispatched: number;
}

@Injectable({ providedIn: 'root' })
export class WorkersService {
  private api = inject(ApiService);

  getWorkers(project: string): Observable<Worker[]> {
    return this.api.get<Worker[]>(`projects/${project}/workers`);
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
    caps?: { enable_fetch: boolean; enable_eval: boolean; enable_build: boolean },
  ): Observable<WorkerRegistration> {
    return this.api.post<WorkerRegistration>(`projects/${project}/workers`, {
      worker_id: workerId,
      display_name: displayName,
      url: url || undefined,
      token: token || undefined,
      enable_fetch: caps?.enable_fetch ?? true,
      enable_eval: caps?.enable_eval ?? true,
      enable_build: caps?.enable_build ?? true,
    });
  }

  setWorkerActive(project: string, workerId: string, active: boolean): Observable<string> {
    return this.api.patch<string>(`projects/${project}/workers/${workerId}`, { active });
  }

  renameWorker(project: string, workerId: string, displayName: string): Observable<string> {
    return this.api.patch<string>(`projects/${project}/workers/${workerId}`, { display_name: displayName });
  }

  setWorkerCapability(
    project: string,
    workerId: string,
    cap: 'fetch' | 'eval' | 'build',
    enabled: boolean,
  ): Observable<string> {
    const body: Record<string, boolean> = {};
    body[`enable_${cap}`] = enabled;
    return this.api.patch<string>(`projects/${project}/workers/${workerId}`, body);
  }

  patchWorker(project: string, workerId: string, body: Record<string, unknown>): Observable<string> {
    return this.api.patch<string>(`projects/${project}/workers/${workerId}`, body);
  }

  deleteWorker(project: string, workerId: string): Observable<string> {
    return this.api.delete<string>(`projects/${project}/workers/${workerId}`);
  }

  testWorker(project: string, workerId: string): Observable<WorkerTestResponse> {
    return this.api.post<WorkerTestResponse>(`projects/${project}/workers/${workerId}/test`, {});
  }
}
