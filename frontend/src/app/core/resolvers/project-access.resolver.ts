/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { inject } from '@angular/core';
import { ResolveFn } from '@angular/router';
import { catchError, map, of } from 'rxjs';
import { ProjectsService } from '@core/services/projects.service';
import { Project } from '@core/models';

export interface ProjectAccessData {
  project: Project | null;
}

export const projectAccessResolver: ResolveFn<ProjectAccessData> = (route) => {
  const projects = inject(ProjectsService);
  const name = route.paramMap.get('project') ?? '';
  if (!name) return of({ project: null });
  return projects.getProject(name).pipe(
    map((project) => ({ project })),
    catchError(() => of({ project: null })),
  );
};
