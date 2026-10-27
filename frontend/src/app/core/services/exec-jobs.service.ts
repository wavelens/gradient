/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable } from 'rxjs';
import { ApiService } from './api.service';
import { ExecJobResponse, ExecJobSummary } from '@core/models';

@Injectable({ providedIn: 'root' })
export class ExecJobsService {
  private api = inject(ApiService);

  forEvaluation(evaluation: string): Observable<ExecJobSummary[]> {
    return this.api.get<ExecJobSummary[]>(`evals/${evaluation}/exec-jobs`);
  }

  get(job: string): Observable<ExecJobResponse> {
    return this.api.get<ExecJobResponse>(`exec-jobs/${job}`);
  }

  log(job: string, attempt?: number): Observable<string> {
    const query = attempt === undefined ? '' : `?attempt=${attempt}`;
    return this.api.get<string>(`exec-jobs/${job}/log${query}`);
  }

  abort(job: string): Observable<string> {
    return this.api.post<string>(`exec-jobs/${job}/abort`);
  }
}
