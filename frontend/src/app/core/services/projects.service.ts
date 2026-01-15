/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable } from 'rxjs';
import { ApiService } from './api.service';
import { Project, ProjectDetail } from '@core/models';

@Injectable({ providedIn: 'root' })
export class ProjectsService {
  private api = inject(ApiService);

  getProjects(organization: string): Observable<Project[]> {
    return this.api.get<Project[]>(`projects/${organization}`);
  }

  getProject(organization: string, project: string): Observable<ProjectDetail> {
    return this.api.get<ProjectDetail>(`projects/${organization}/${project}/details`);
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
  ): Observable<Project> {
    return this.api.put<Project>(`projects/${organization}`, data);
  }

  updateProject(
    organization: string,
    project: string,
    data: Partial<Project>
  ): Observable<Project> {
    return this.api.patch<Project>(`projects/${organization}/${project}`, data);
  }

  deleteProject(organization: string, project: string): Observable<void> {
    return this.api.delete<void>(`projects/${organization}/${project}`);
  }

  startEvaluation(organization: string, project: string): Observable<void> {
    return this.api.post<void>(`projects/${organization}/${project}/evaluate`);
  }

  activateProject(organization: string, project: string): Observable<void> {
    return this.api.post<void>(`projects/${organization}/${project}/active`);
  }

  deactivateProject(organization: string, project: string): Observable<void> {
    return this.api.delete<void>(`projects/${organization}/${project}/active`);
  }
}
