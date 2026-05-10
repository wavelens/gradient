/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { inject } from '@angular/core';
import { ResolveFn } from '@angular/router';
import { map } from 'rxjs';
import { ProjectsService } from '@core/services/projects.service';
import { ProjectDetail } from '@core/models/project.model';
import { AccessState, accessFromEntity } from '@core/models/access.model';

export interface ProjectAccessData {
  project: ProjectDetail;
  access: AccessState;
}

export const projectAccessResolver: ResolveFn<ProjectAccessData> = (route) => {
  const projects = inject(ProjectsService);
  const org = route.paramMap.get('org') ?? '';
  const project = route.paramMap.get('project') ?? '';
  return projects.getProject(org, project).pipe(
    map((p) => ({ project: p, access: accessFromEntity(p) })),
  );
};
