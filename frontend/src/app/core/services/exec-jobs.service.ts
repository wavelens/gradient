/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable } from 'rxjs';
import { environment } from '@environments/environment';
import { ApiService } from './api.service';
import { ExecJobOutputView, ExecJobPullRequestView, ExecJobResponse, ExecJobSummary } from '@core/models';

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

  outputs(job: string): Observable<ExecJobOutputView[]> {
    return this.api.get<ExecJobOutputView[]>(`exec-jobs/${job}/outputs`);
  }

  outputUrl(job: string, path: string): string {
    const encoded = path.split('/').map(encodeURIComponent).join('/');
    return `${environment.apiUrl}/exec-jobs/${job}/outputs/${encoded}`;
  }

  pullRequests(job: string): Observable<ExecJobPullRequestView[]> {
    return this.api.get<ExecJobPullRequestView[]>(`exec-jobs/${job}/pull-requests`);
  }

  abort(job: string): Observable<string> {
    return this.api.post<string>(`exec-jobs/${job}/abort`);
  }
}
