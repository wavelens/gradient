/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable } from 'rxjs';
import { ApiService } from './api.service';
import { Project, ProjectDetail, EntryPointSummary } from '@core/models';

@Injectable({ providedIn: 'root' })
export class ProjectsService {
  private api = inject(ApiService);

  getProjects(organization: string): Observable<Project[]> {
    return this.api.get<Project[]>(`projects/${organization}`);
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

  getEntryPoints(organization: string, project: string): Observable<EntryPointSummary[]> {
    return this.api.get<EntryPointSummary[]>(`projects/${organization}/${project}/entry-points`);
  }

  startEvaluation(organization: string, project: string): Observable<string> {
    return this.api.post<string>(`projects/${organization}/${project}/evaluate`);
  }

  abortEvaluation(organization: string, project: string, evaluationId: string): Observable<string> {
    return this.api.post<string>(`evals/${evaluationId}`, { method: 'abort' });
  }

  activateProject(organization: string, project: string): Observable<string> {
    return this.api.post<string>(`projects/${organization}/${project}/active`);
  }

  deactivateProject(organization: string, project: string): Observable<string> {
    return this.api.delete<string>(`projects/${organization}/${project}/active`);
  }
}
