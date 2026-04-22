/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable } from 'rxjs';
import { ApiService } from './api.service';
import { Worker, WorkerRegistration } from '@core/models';

@Injectable({ providedIn: 'root' })
export class WorkersService {
  private api = inject(ApiService);

  getWorkers(org: string): Observable<Worker[]> {
    return this.api.get<Worker[]>(`orgs/${org}/workers`);
  }

  registerWorker(org: string, workerId: string, displayName: string, url?: string, token?: string): Observable<WorkerRegistration> {
    return this.api.post<WorkerRegistration>(`orgs/${org}/workers`, {
      worker_id: workerId,
      display_name: displayName,
      url: url || undefined,
      token: token || undefined,
    });
  }

  setWorkerActive(org: string, workerId: string, active: boolean): Observable<string> {
    return this.api.patch<string>(`orgs/${org}/workers/${workerId}`, { active });
  }

  renameWorker(org: string, workerId: string, displayName: string): Observable<string> {
    return this.api.patch<string>(`orgs/${org}/workers/${workerId}`, { display_name: displayName });
  }

  deleteWorker(org: string, workerId: string): Observable<string> {
    return this.api.delete<string>(`orgs/${org}/workers/${workerId}`);
  }
}
