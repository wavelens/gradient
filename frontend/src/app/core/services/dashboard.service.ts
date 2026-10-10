/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable } from 'rxjs';
import { ApiService } from './api.service';
import { ActivityDay, DashboardFilter, DashboardStats, Rail, TasksPage } from '@core/models';

@Injectable({ providedIn: 'root' })
export class DashboardService {
  private api = inject(ApiService);

  stats(): Observable<DashboardStats> {
    return this.api.get<DashboardStats>('dashboard/stats');
  }

  tasks(filter: DashboardFilter | null, page: number, perPage: number, history: number): Observable<TasksPage> {
    const chosen = filter ? `filter=${filter}&` : '';
    return this.api.get<TasksPage>(`dashboard/tasks?${chosen}page=${page}&per_page=${perPage}&history=${history}`);
  }

  activity(): Observable<{ days: ActivityDay[] }> {
    return this.api.get<{ days: ActivityDay[] }>('dashboard/activity');
  }

  rail(): Observable<Rail> {
    return this.api.get<Rail>('dashboard/rail');
  }
}
