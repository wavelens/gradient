/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { inject } from '@angular/core';
import { RedirectCommand, ResolveFn, Router, UrlSegment } from '@angular/router';
import { Observable, map, of, switchMap } from 'rxjs';
import { EvaluationsService } from '@core/services/evaluations.service';
import { TasksService } from '@core/services/tasks.service';
import { EvaluationResponse, TaskDetailsResponse } from '@core/models';
import { DeniedTarget, deniedPage, denyOnRefusal } from '@core/routing/access-denied';
import { links } from '@shared/routing';

export interface EvaluationWithOptionalTask {
  evaluation: EvaluationResponse;
  task: TaskDetailsResponse | null;
}

export function redirectToCorrectProjectSlug(router: Router, url: string, project: string): RedirectCommand {
  const tree = router.parseUrl(url);
  const group = tree.root.children['primary'];
  if (group) group.segments = group.segments.map((segment, index) => (index === 1 ? new UrlSegment(project, {}) : segment));
  return new RedirectCommand(tree, { replaceUrl: true });
}

export const evaluationAccessResolver: ResolveFn<EvaluationWithOptionalTask> = (route, state) => {
  const router = inject(Router);
  const tasks = inject(TasksService);
  const project = route.paramMap.get('project') ?? '';
  const id = route.paramMap.get('evaluationId') ?? '';
  const target: DeniedTarget = { kind: 'evaluation', back: String(links.projects()) };
  const withTask = (evaluation: EvaluationResponse): Observable<EvaluationWithOptionalTask | RedirectCommand> => {
    const task$: Observable<TaskDetailsResponse | null> = evaluation.task_name ? tasks.getTask(project, evaluation.task_name) : of(null);
    return task$.pipe(map((task) => (!task || task.id === evaluation.task ? { evaluation, task } : deniedPage(router, { ...target, back: String(links.project(project)) }, state.url))));
  };
  return inject(EvaluationsService).getEvaluation(id).pipe(
    switchMap((evaluation) => evaluation.project_name === project
      ? withTask(evaluation)
      : of(redirectToCorrectProjectSlug(router, state.url, evaluation.project_name))),
    denyOnRefusal<EvaluationWithOptionalTask | RedirectCommand>(target, state.url, project),
  );
};
