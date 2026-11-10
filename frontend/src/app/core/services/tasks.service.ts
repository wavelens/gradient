/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable } from 'rxjs';
import { HttpResponse } from '@angular/common/http';
import { ApiService } from './api.service';
import {
  CheckRepositoryRequest,
  CreatedEntityResponse,
  InstallDeployKeyRequest,
  InstallWebhookRequest,
  Paginated,
  TaskResponse,
  TaskDetailsResponse,
  PaginatedEntryPoints,
  EvaluationSummary,
  WalkMode,
  TaskMetricsResponse,
  EntryPointMetricsResponse,
  MakeTaskRequest,
  PatchTaskRequest,
  TaskRepositoryResponse,
  RepositoryAccessCheckRequest,
  DuplicateTaskRequest,
} from '@core/models';

@Injectable({ providedIn: 'root' })
export class TasksService {
  private api = inject(ApiService);

  checkTaskNameAvailable(project: string, name: string): Observable<boolean> {
    return this.api.get<boolean>(`tasks/${project}/available?name=${encodeURIComponent(name)}`);
  }

  getTasks(project: string, page = 1, perPage = 50): Observable<Paginated<TaskResponse[]>> {
    return this.api.get<Paginated<TaskResponse[]>>(`tasks/${project}?page=${page}&per_page=${perPage}`);
  }

  getTask(project: string, task: string): Observable<TaskDetailsResponse> {
    return this.api.get<TaskDetailsResponse>(`tasks/${project}/${task}/details`);
  }

  getTaskInfo(project: string, task: string): Observable<TaskResponse> {
    return this.api.get<TaskResponse>(`tasks/${project}/${task}`);
  }

  createTask(
    project: string,
    data: MakeTaskRequest
  ): Observable<CreatedEntityResponse> {
    return this.api.put<CreatedEntityResponse>(`tasks/${project}`, data);
  }

  updateTask(
    project: string,
    task: string,
    data: PatchTaskRequest
  ): Observable<string> {
    return this.api.patch<string>(`tasks/${project}/${task}`, data);
  }

  deleteTask(project: string, task: string): Observable<string> {
    return this.api.delete<string>(`tasks/${project}/${task}`);
  }

  deleteTaskEvaluationsBeforeDate(project: string, task: string, before: string): Observable<number> {
    return this.api.delete<number>(`tasks/${project}/${task}/evaluations?before=${encodeURIComponent(before)}`);
  }

  getEntryPoints(project: string, task: string, evaluationId?: string, limit?: number, offset?: number, search?: string): Observable<PaginatedEntryPoints> {
    const params: string[] = [];
    if (evaluationId) params.push(`evaluation_id=${evaluationId}`);
    if (limit !== undefined) params.push(`limit=${limit}`);
    if (offset !== undefined) params.push(`offset=${offset}`);
    if (search) params.push(`search=${encodeURIComponent(search)}`);
    const query = params.length ? `?${params.join('&')}` : '';
    return this.api.get<PaginatedEntryPoints>(`tasks/${project}/${task}/entry-points${query}`);
  }

  getEvaluations(project: string, task: string, limit?: number, before?: string): Observable<EvaluationSummary[]> {
    const params: string[] = [];
    if (limit) params.push(`limit=${limit}`);
    if (before) params.push(`before=${before}`);
    const q = params.length ? `?${params.join('&')}` : '';
    return this.api.get<EvaluationSummary[]>(`tasks/${project}/${task}/evaluations${q}`);
  }

  startEvaluation(project: string, task: string, walk: WalkMode = 'pruned'): Observable<CreatedEntityResponse> {
    const body = walk === 'pruned' ? undefined : { walk };
    return this.api.post<CreatedEntityResponse>(`tasks/${project}/${task}/evaluate`, body);
  }

  abortEvaluation(project: string, task: string, evaluationId: string): Observable<string> {
    return this.api.post<string>(`evals/${evaluationId}`, { method: 'abort' });
  }

  transferOwnership(project: string, task: string, targetProject: string): Observable<string> {
    return this.api.post<string>(`tasks/${project}/${task}/transfer`, { project: targetProject });
  }

  activateTask(project: string, task: string): Observable<string> {
    return this.api.post<string>(`tasks/${project}/${task}/active`);
  }

  checkRepository(project: string, task: string, repository: string): Observable<string> {
    const body: CheckRepositoryRequest = { repository };
    return this.api.post<string>(`tasks/${project}/${task}/check-repository`, body);
  }

  getRepository(project: string, task: string): Observable<TaskRepositoryResponse> {
    return this.api.get<TaskRepositoryResponse>(`tasks/${project}/${task}/repository`);
  }

  testRepositoryCloneAccess(project: string, repository: string): Observable<string> {
    const body: RepositoryAccessCheckRequest = { repository };
    return this.api.post<string>(`projects/${project}/repository-check`, body);
  }

  duplicateTask(project: string, task: string, body: DuplicateTaskRequest): Observable<string> {
    return this.api.post<string>(`tasks/${project}/${task}/duplicate`, body);
  }

  installWebhook(project: string, task: string, body: InstallWebhookRequest): Observable<string> {
    return this.api.post<string>(`tasks/${project}/${task}/repository/install-webhook`, body);
  }

  installDeployKey(project: string, task: string, body: InstallDeployKeyRequest): Observable<string> {
    return this.api.post<string>(`tasks/${project}/${task}/repository/install-deploy-key`, body);
  }

  deactivateTask(project: string, task: string): Observable<string> {
    return this.api.delete<string>(`tasks/${project}/${task}/active`);
  }

  getTaskMetrics(project: string, task: string): Observable<TaskMetricsResponse> {
    return this.api.get<TaskMetricsResponse>(`metrics/tasks/${project}/${task}/evaluations`);
  }

  getEntryPointMetrics(project: string, task: string, eval_attr: string): Observable<EntryPointMetricsResponse> {
    return this.api.get<EntryPointMetricsResponse>(
      `metrics/tasks/${project}/${task}/entry-point?eval=${encodeURIComponent(eval_attr)}`
    );
  }

  /// The dialog asks what to include; the endpoint asks what to anonymise. The
  /// inversion lives here so no double negative reaches the UI.
  downloadReport(evaluationId: string, options: ReportOptions): Observable<HttpResponse<Blob>> {
    const query = new URLSearchParams({
      anonymize_identities: String(!options.include_identities),
      anonymize_packages: String(!options.include_packages),
      include_logs: String(options.include_logs),
      include_instance: String(options.include_instance),
      include_exec: String(options.include_exec),
    });
    return this.api.getBlob(`evals/${evaluationId}/report?${query}`);
  }
}

export interface ReportOptions {
  include_identities: boolean;
  include_packages: boolean;
  include_logs: boolean;
  include_instance: boolean;
  include_exec: boolean;
}

