/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable } from 'rxjs';
import { ApiService } from './api.service';
import { Project, ProjectDetail, EntryPointSummary, Paginated } from '@core/models';

@Injectable({ providedIn: 'root' })
export class ProjectsService {
  private api = inject(ApiService);

  checkProjectNameAvailable(organization: string, name: string): Observable<boolean> {
    return this.api.get<boolean>(`projects/${organization}/available?name=${encodeURIComponent(name)}`);
  }

  getProjects(organization: string, page = 1, perPage = 50): Observable<Paginated<Project[]>> {
    return this.api.get<Paginated<Project[]>>(`projects/${organization}?page=${page}&per_page=${perPage}`);
  }

  getProject(organization: string, project: string): Observable<ProjectDetail> {
    return this.api.get<ProjectDetail>(`projects/${organization}/${project}/details`);
  }

  getProjectInfo(organization: string, project: string): Observable<Project> {
    return this.api.get<Project>(`projects/${organization}/${project}`);
  }

  createProject(
    organization: string,
    data: {
      name: string;
      display_name: string;
      description: string;
      repository: string;
      evaluation_wildcard: string;
    }
  ): Observable<string> {
    return this.api.put<string>(`projects/${organization}`, data);
  }

  updateProject(
    organization: string,
    project: string,
    data: Partial<Project>
  ): Observable<string> {
    return this.api.patch<string>(`projects/${organization}/${project}`, data);
  }

  deleteProject(organization: string, project: string): Observable<string> {
    return this.api.delete<string>(`projects/${organization}/${project}`);
  }

  getEntryPoints(organization: string, project: string, evaluationId?: string): Observable<EntryPointSummary[]> {
    const url = evaluationId
      ? `projects/${organization}/${project}/entry-points?evaluation_id=${evaluationId}`
      : `projects/${organization}/${project}/entry-points`;
    return this.api.get<EntryPointSummary[]>(url);
  }

  startEvaluation(organization: string, project: string): Observable<string> {
    return this.api.post<string>(`projects/${organization}/${project}/evaluate`);
  }

  restartFailedBuilds(organization: string, project: string): Observable<string> {
    return this.api.post<string>(`projects/${organization}/${project}/evaluate`, { mode: 'restart_failed' });
  }

  abortEvaluation(organization: string, project: string, evaluationId: string): Observable<string> {
    return this.api.post<string>(`evals/${evaluationId}`, { method: 'abort' });
  }

  transferOwnership(organization: string, project: string, targetOrg: string): Observable<string> {
    return this.api.post<string>(`projects/${organization}/${project}/transfer`, { organization: targetOrg });
  }

  activateProject(organization: string, project: string): Observable<string> {
    return this.api.post<string>(`projects/${organization}/${project}/active`);
  }

  deactivateProject(organization: string, project: string): Observable<string> {
    return this.api.delete<string>(`projects/${organization}/${project}/active`);
  }

  getProjectMetrics(organization: string, project: string): Observable<ProjectMetricsResponse> {
    return this.api.get<ProjectMetricsResponse>(`projects/${organization}/${project}/metrics`);
  }

  getEntryPointMetrics(organization: string, project: string, eval_attr: string): Observable<EntryPointMetricsResponse> {
    return this.api.get<EntryPointMetricsResponse>(
      `projects/${organization}/${project}/entry-point-metrics?eval=${encodeURIComponent(eval_attr)}`
    );
  }
}

export interface ProjectMetricPoint {
  evaluation_id: string;
  created_at: string;
  build_time_total_ms: number;
  eval_time_ms: number;
  output_size_bytes: number | null;
  closure_size_bytes: number | null;
  dependencies_count: number;
}

export interface ProjectMetricsResponse {
  keep_evaluations: number;
  points: ProjectMetricPoint[];
}

export interface EntryPointMetricPoint {
  evaluation_id: string;
  created_at: string;
  build_status: string;
  build_time_ms: number | null;
  output_size_bytes: number | null;
  closure_size_bytes: number | null;
  dependencies_count: number;
}

export interface EntryPointMetricsResponse {
  eval: string;
  keep_evaluations: number;
  points: EntryPointMetricPoint[];
}
