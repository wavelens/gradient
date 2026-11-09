/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable } from 'rxjs';
import { asSectionRequest } from '@core/interceptors/request-context';
import { ApiService } from './api.service';
import { Activity, DashboardStats, Filter, TasksPage, Rail } from '@core/models';

@Injectable({ providedIn: 'root' })
export class DashboardService {
  private api = inject(ApiService);

  stats(): Observable<DashboardStats> {
    return this.api.get<DashboardStats>('dashboard/stats', asSectionRequest());
  }

  tasks(filter: Filter | null, page: number, perPage: number, history: number): Observable<TasksPage> {
    const chosen = filter ? `filter=${filter}&` : '';
    return this.api.get<TasksPage>(`dashboard/tasks?${chosen}page=${page}&per_page=${perPage}&history=${history}`, asSectionRequest());
  }

  activity(): Observable<Activity> {
    return this.api.get<Activity>(`dashboard/activity?tz_offset_minutes=${-new Date().getTimezoneOffset()}`, asSectionRequest());
  }

  rail(): Observable<Rail> {
    return this.api.get<Rail>('dashboard/rail', asSectionRequest());
  }
}
