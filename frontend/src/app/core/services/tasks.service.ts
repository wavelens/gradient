/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable } from 'rxjs';
import { HttpResponse } from '@angular/common/http';
import { ApiService } from './api.service';
import { Task, TaskDetail, EntryPointSummary, EvaluationSummary, Paginated } from '@core/models';

@Injectable({ providedIn: 'root' })
export class TasksService {
  private api = inject(ApiService);

  checkTaskNameAvailable(project: string, name: string): Observable<boolean> {
    return this.api.get<boolean>(`tasks/${project}/available?name=${encodeURIComponent(name)}`);
  }

  getTasks(project: string, page = 1, perPage = 50): Observable<Paginated<Task[]>> {
    return this.api.get<Paginated<Task[]>>(`tasks/${project}?page=${page}&per_page=${perPage}`);
  }

  getTask(project: string, task: string): Observable<TaskDetail> {
    return this.api.get<TaskDetail>(`tasks/${project}/${task}/details`);
  }

  getTaskInfo(project: string, task: string): Observable<Task> {
    return this.api.get<Task>(`tasks/${project}/${task}`);
  }

  createTask(
    project: string,
    data: {
      name: string;
      display_name: string;
      description: string;
      repository: string;
      wildcard: string;
    }
  ): Observable<string> {
    return this.api.put<string>(`tasks/${project}`, data);
  }

  updateTask(
    project: string,
    task: string,
    data: Partial<Task>
  ): Observable<string> {
    return this.api.patch<string>(`tasks/${project}/${task}`, data);
  }

  deleteTask(project: string, task: string): Observable<string> {
    return this.api.delete<string>(`tasks/${project}/${task}`);
  }

  getEntryPoints(project: string, task: string, evaluationId?: string): Observable<EntryPointSummary[]> {
    const url = evaluationId
      ? `tasks/${project}/${task}/entry-points?evaluation_id=${evaluationId}`
      : `tasks/${project}/${task}/entry-points`;
    return this.api.get<EntryPointSummary[]>(url);
  }

  getEvaluations(project: string, task: string, limit?: number): Observable<EvaluationSummary[]> {
    const q = limit ? `?limit=${limit}` : '';
    return this.api.get<EvaluationSummary[]>(`tasks/${project}/${task}/evaluations${q}`);
  }

  startEvaluation(project: string, task: string): Observable<string> {
    return this.api.post<string>(`tasks/${project}/${task}/evaluate`);
  }

  restartFailedBuilds(project: string, task: string): Observable<string> {
    return this.api.post<string>(`tasks/${project}/${task}/evaluate`, { mode: 'restart_failed' });
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
    });
    return this.api.getBlob(`evals/${evaluationId}/report?${query}`);
  }
}

export interface ReportOptions {
  include_identities: boolean;
  include_packages: boolean;
  include_logs: boolean;
  include_instance: boolean;
}

export interface TaskMetricPoint {
  evaluation_id: string;
  created_at: string;
  build_time_total_ms: number;
  eval_time_ms: number;
  output_size_bytes: number | null;
  closure_size_bytes: number | null;
  runtime_closure_size_bytes: number | null;
  dependencies_count: number;
}

export interface TaskMetricsResponse {
  keep_evaluations: number;
  points: TaskMetricPoint[];
}

export interface EntryPointMetricPoint {
  evaluation_id: string;
  build_id: string;
  created_at: string;
  build_status: string;
  build_time_ms: number | null;
  output_size_bytes: number | null;
  closure_size_bytes: number | null;
  runtime_closure_size_bytes: number | null;
  dependencies_count: number;
}

export interface EntryPointMetricsResponse {
  eval: string;
  keep_evaluations: number;
  points: EntryPointMetricPoint[];
}
