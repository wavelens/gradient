/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Signal, inject } from '@angular/core';
import { toSignal } from '@angular/core/rxjs-interop';
import { ActivatedRoute, Data, ResolveFn } from '@angular/router';
import { map, tap } from 'rxjs';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { ProjectsService } from '@core/services/projects.service';
import { ProjectResponse } from '@core/models';
import { denyOnRefusal } from '@core/routing/access-denied';

export interface ProjectAccessData {
  project: ProjectResponse;
}

export const projectAccessResolver: ResolveFn<ProjectAccessData> = (route, state) => {
  const projects = inject(ProjectsService);
  const crumbs = inject(BreadcrumbsService);
  const name = route.paramMap.get('project') ?? '';
  return projects.getProject(name).pipe(
    tap((project) => crumbs.rememberProject(name, project.display_name)),
    map((project) => ({ project })),
    denyOnRefusal({ kind: 'project', name, back: '/projects' }, state.url),
  );
};

const resolvedProject = (data: Data): ProjectResponse | undefined =>
  (data['projectAccess'] as ProjectAccessData | undefined)?.project;

export function injectProject(): Signal<ProjectResponse | undefined> {
  const route = inject(ActivatedRoute);
  return toSignal(route.data.pipe(map(resolvedProject)), { initialValue: resolvedProject(route.snapshot.data) });
}
