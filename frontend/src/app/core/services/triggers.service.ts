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

  private base(org: string, proj: string): string {
    return `tasks/${org}/${proj}/triggers`;
  }

  list(org: string, proj: string): Observable<TaskTrigger[]> {
    return this.api.get<TaskTrigger[]>(this.base(org, proj));
  }

  create(org: string, proj: string, body: CreateTriggerBody): Observable<TaskTrigger> {
    return this.api.post<TaskTrigger>(this.base(org, proj), body);
  }

  get(org: string, proj: string, id: string): Observable<TaskTrigger> {
    return this.api.get<TaskTrigger>(`${this.base(org, proj)}/${id}`);
  }

  update(org: string, proj: string, id: string, body: UpdateTriggerBody): Observable<TaskTrigger> {
    return this.api.patch<TaskTrigger>(`${this.base(org, proj)}/${id}`, body);
  }

  delete(org: string, proj: string, id: string): Observable<boolean> {
    return this.api.delete<boolean>(`${this.base(org, proj)}/${id}`);
  }

  fireNow(org: string, proj: string, id: string): Observable<boolean> {
    return this.api.post<boolean>(`${this.base(org, proj)}/${id}/test`);
  }
}
