/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable } from 'rxjs';
import { ApiService } from './api.service';
import { RunnerJobResponse, RunnerJobSummary } from '@core/models';

@Injectable({ providedIn: 'root' })
export class RunnerJobsService {
  private api = inject(ApiService);

  forEvaluation(evaluation: string): Observable<RunnerJobSummary[]> {
    return this.api.get<RunnerJobSummary[]>(`evals/${evaluation}/runner-jobs`);
  }

  get(job: string): Observable<RunnerJobResponse> {
    return this.api.get<RunnerJobResponse>(`runner-jobs/${job}`);
  }

  log(job: string, attempt?: number): Observable<string> {
    const query = attempt === undefined ? '' : `?attempt=${attempt}`;
    return this.api.get<string>(`runner-jobs/${job}/log${query}`);
  }

  abort(job: string): Observable<string> {
    return this.api.post<string>(`runner-jobs/${job}/abort`);
  }
}
