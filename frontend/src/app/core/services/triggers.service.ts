/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable } from 'rxjs';
import { ApiService } from './api.service';
import { CreateTriggerBody, TaskTrigger, UpdateTriggerBody } from '@core/models';

@Injectable({ providedIn: 'root' })
export class TriggersService {
  private api = inject(ApiService);

  private base(project: string, proj: string): string {
    return `tasks/${project}/${proj}/triggers`;
  }

  list(project: string, proj: string): Observable<TaskTrigger[]> {
    return this.api.get<TaskTrigger[]>(this.base(project, proj));
  }

  create(project: string, proj: string, body: CreateTriggerBody): Observable<TaskTrigger> {
    return this.api.post<TaskTrigger>(this.base(project, proj), body);
  }

  get(project: string, proj: string, id: string): Observable<TaskTrigger> {
    return this.api.get<TaskTrigger>(`${this.base(project, proj)}/${id}`);
  }

  update(project: string, proj: string, id: string, body: UpdateTriggerBody): Observable<TaskTrigger> {
    return this.api.patch<TaskTrigger>(`${this.base(project, proj)}/${id}`, body);
  }

  delete(project: string, proj: string, id: string): Observable<boolean> {
    return this.api.delete<boolean>(`${this.base(project, proj)}/${id}`);
  }

  fireNow(project: string, proj: string, id: string): Observable<boolean> {
    return this.api.post<boolean>(`${this.base(project, proj)}/${id}/test`);
  }
}
