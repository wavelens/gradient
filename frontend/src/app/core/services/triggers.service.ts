/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable } from 'rxjs';
import { ApiService } from './api.service';
import {
  DeletedResponse,
  TriggerOut,
  CreateTriggerRequest,
  UpdateTriggerRequest,
} from '@core/models';

@Injectable({ providedIn: 'root' })
export class TriggersService {
  private api = inject(ApiService);

  private base(project: string, proj: string): string {
    return `tasks/${project}/${proj}/triggers`;
  }

  list(project: string, proj: string): Observable<TriggerOut[]> {
    return this.api.get<TriggerOut[]>(this.base(project, proj));
  }

  create(project: string, proj: string, body: CreateTriggerRequest): Observable<TriggerOut> {
    return this.api.post<TriggerOut>(this.base(project, proj), body);
  }

  get(project: string, proj: string, id: string): Observable<TriggerOut> {
    return this.api.get<TriggerOut>(`${this.base(project, proj)}/${id}`);
  }

  update(project: string, proj: string, id: string, body: UpdateTriggerRequest): Observable<TriggerOut> {
    return this.api.patch<TriggerOut>(`${this.base(project, proj)}/${id}`, body);
  }

  delete(project: string, proj: string, id: string): Observable<DeletedResponse> {
    return this.api.delete<DeletedResponse>(`${this.base(project, proj)}/${id}`);
  }

  fireNow(project: string, proj: string, id: string): Observable<boolean> {
    return this.api.post<boolean>(`${this.base(project, proj)}/${id}/test`);
  }
}
