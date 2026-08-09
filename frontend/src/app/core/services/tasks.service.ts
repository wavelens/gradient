/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable } from 'rxjs';
import { ApiService } from './api.service';
import { Task, TaskDetail, EntryPointSummary, EvaluationSummary, Paginated } from '@core/models';

@Injectable({ providedIn: 'root' })
export class TasksService {
  private api = inject(ApiService);

  checkTaskNameAvailable(organization: string, name: string): Observable<boolean> {
    return this.api.get<boolean>(`tasks/${organization}/available?name=${encodeURIComponent(name)}`);
  }

  getTasks(organization: string, page = 1, perPage = 50): Observable<Paginated<Task[]>> {
    return this.api.get<Paginated<Task[]>>(`tasks/${organization}?page=${page}&per_page=${perPage}`);
  }

  getTask(organization: string, task: string): Observable<TaskDetail> {
    return this.api.get<TaskDetail>(`tasks/${organization}/${task}/details`);
  }

  getTaskInfo(organization: string, task: string): Observable<Task> {
    return this.api.get<Task>(`tasks/${organization}/${task}`);
  }

  createTask(
    organization: string,
    data: {
      name: string;
      display_name: string;
      description: string;
      repository: string;
      wildcard: string;
    }
  ): Observable<string> {
    return this.api.put<string>(`tasks/${organization}`, data);
  }

  updateTask(
    organization: string,
    task: string,
    data: Partial<Task>
  ): Observable<string> {
    return this.api.patch<string>(`tasks/${organization}/${task}`, data);
  }

  deleteTask(organization: string, task: string): Observable<string> {
    return this.api.delete<string>(`tasks/${organization}/${task}`);
  }

  getEntryPoints(organization: string, task: string, evaluationId?: string): Observable<EntryPointSummary[]> {
    const url = evaluationId
      ? `tasks/${organization}/${task}/entry-points?evaluation_id=${evaluationId}`
      : `tasks/${organization}/${task}/entry-points`;
    return this.api.get<EntryPointSummary[]>(url);
  }

  getEvaluations(organization: string, task: string, limit?: number): Observable<EvaluationSummary[]> {
    const q = limit ? `?limit=${limit}` : '';
    return this.api.get<EvaluationSummary[]>(`tasks/${organization}/${task}/evaluations${q}`);
  }

  startEvaluation(organization: string, task: string): Observable<string> {
    return this.api.post<string>(`tasks/${organization}/${task}/evaluate`);
  }

  restartFailedBuilds(organization: string, task: string): Observable<string> {
    return this.api.post<string>(`tasks/${organization}/${task}/evaluate`, { mode: 'restart_failed' });
  }

  abortEvaluation(organization: string, task: string, evaluationId: string): Observable<string> {
    return this.api.post<string>(`evals/${evaluationId}`, { method: 'abort' });
  }

  transferOwnership(organization: string, task: string, targetOrg: string): Observable<string> {
    return this.api.post<string>(`tasks/${organization}/${task}/transfer`, { organization: targetOrg });
  }

  activateTask(organization: string, task: string): Observable<string> {
    return this.api.post<string>(`tasks/${organization}/${task}/active`);
  }

  deactivateTask(organization: string, task: string): Observable<string> {
    return this.api.delete<string>(`tasks/${organization}/${task}/active`);
  }

  getTaskMetrics(organization: string, task: string): Observable<TaskMetricsResponse> {
    return this.api.get<TaskMetricsResponse>(`metrics/tasks/${organization}/${task}/evaluations`);
  }

  getEntryPointMetrics(organization: string, task: string, eval_attr: string): Observable<EntryPointMetricsResponse> {
    return this.api.get<EntryPointMetricsResponse>(
      `metrics/tasks/${organization}/${task}/entry-point?eval=${encodeURIComponent(eval_attr)}`
    );
  }
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
