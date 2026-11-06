/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable } from 'rxjs';
import { environment } from '@environments/environment';
import { ApiService, RequestOptions } from './api.service';
import { ExecJobOutputView, ExecJobPullRequestView, ExecJobResponse, ExecJobSummary } from '@core/models';

function query(params: Record<string, number | undefined>): string {
  const defined = Object.entries(params).filter((entry): entry is [string, number] => entry[1] !== undefined);
  return defined.length === 0 ? '' : `?${new URLSearchParams(defined.map(([key, value]) => [key, String(value)]))}`;
}

@Injectable({ providedIn: 'root' })
export class ExecJobsService {
  private api = inject(ApiService);

  forEvaluation(evaluation: string, options?: RequestOptions): Observable<ExecJobSummary[]> {
    return this.api.get<ExecJobSummary[]>(`evals/${evaluation}/exec`, options);
  }

  get(job: string, options?: RequestOptions): Observable<ExecJobResponse> {
    return this.api.get<ExecJobResponse>(`exec/${job}`, options);
  }

  log(job: string, attempt?: number, node?: number, options?: RequestOptions): Observable<string> {
    return this.api.get<string>(`exec/${job}/log${query({ attempt, node })}`, options);
  }

  outputs(job: string, node?: number, options?: RequestOptions): Observable<ExecJobOutputView[]> {
    return this.api.get<ExecJobOutputView[]>(`exec/${job}/outputs${query({ node })}`, options);
  }

  outputUrl(job: string, path: string, node?: number): string {
    const encoded = path.split('/').map(encodeURIComponent).join('/');
    return `${environment.apiUrl}/exec/${job}/outputs/${encoded}${query({ node })}`;
  }

  pullRequests(job: string, options?: RequestOptions): Observable<ExecJobPullRequestView[]> {
    return this.api.get<ExecJobPullRequestView[]>(`exec/${job}/pull-requests`, options);
  }

  abort(job: string): Observable<string> {
    return this.api.post<string>(`exec/${job}/abort`);
  }

  run(job: string): Observable<string> {
    return this.api.post<string>(`exec/${job}/run`);
  }

  keepVmRunning(job: string): Observable<string> {
    return this.api.post<string>(`exec/${job}/keep-vm-running`);
  }

  letVmStop(job: string): Observable<string> {
    return this.api.delete<string>(`exec/${job}/keep-vm-running`);
  }
}
