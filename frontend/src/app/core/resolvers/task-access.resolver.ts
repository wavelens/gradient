/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { inject } from '@angular/core';
import { ResolveFn } from '@angular/router';
import { map, tap } from 'rxjs';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { TasksService } from '@core/services/tasks.service';
import { TaskDetail } from '@core/models/task.model';
import { AccessState, accessFromEntity } from '@core/models/access.model';

export interface TaskAccessData {
  task: TaskDetail;
  access: AccessState;
}

export const taskAccessResolver: ResolveFn<TaskAccessData> = (route) => {
  const tasks = inject(TasksService);
  const crumbs = inject(BreadcrumbsService);
  const project = route.paramMap.get('project') ?? '';
  const task = route.paramMap.get('task') ?? '';
  return tasks.getTask(project, task).pipe(
    tap((p) => crumbs.rememberTask(project, task, p.display_name)),
    map((p) => ({ task: p, access: accessFromEntity(p) })),
  );
};
