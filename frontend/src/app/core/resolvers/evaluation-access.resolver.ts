/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { inject } from '@angular/core';
import { RedirectCommand, ResolveFn, Router } from '@angular/router';
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

export const evaluationAccessResolver: ResolveFn<EvaluationWithOptionalTask> = (route, state) => {
  const router = inject(Router);
  const tasks = inject(TasksService);
  const project = route.paramMap.get('project') ?? '';
  const id = route.paramMap.get('evaluationId') ?? '';
  const target: DeniedTarget = { kind: 'evaluation', name: id, back: String(links.project(project)) };
  return inject(EvaluationsService).getEvaluation(id).pipe(
    switchMap((evaluation): Observable<EvaluationWithOptionalTask> => evaluation.task_name
      ? tasks.getTask(project, evaluation.task_name).pipe(map((task) => ({ evaluation, task })))
      : of({ evaluation, task: null })),
    map((data): EvaluationWithOptionalTask | RedirectCommand =>
      !data.task || data.task.id === data.evaluation.task ? data : deniedPage(router, target, state.url)),
    denyOnRefusal<EvaluationWithOptionalTask | RedirectCommand>(target, state.url),
  );
};
