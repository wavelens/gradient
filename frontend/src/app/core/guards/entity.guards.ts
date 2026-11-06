/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { inject } from '@angular/core';
import { CanActivateFn, Router } from '@angular/router';
import { map, of, switchMap } from 'rxjs';
import { EvaluationsService } from '@core/services/evaluations.service';
import { ExecJobsService } from '@core/services/exec-jobs.service';
import { TasksService } from '@core/services/tasks.service';
import { TeamsService } from '@core/services/teams.service';
import { WorkersService } from '@core/services/workers.service';
import { DeniedTarget, deniedPage, denyOnRefusal } from '@core/routing/access-denied';

export const visibleTeamGuard: CanActivateFn = (route, state) => {
  const name = route.paramMap.get('team') ?? '';
  return inject(TeamsService).get(name).pipe(
    map(() => true as const),
    denyOnRefusal({ kind: 'team', name, back: '/teams' }, state.url),
  );
};

export const evaluationInProjectGuard: CanActivateFn = (route, state) => {
  const router = inject(Router);
  const tasks = inject(TasksService);
  const project = route.paramMap.get('project') ?? '';
  const id = route.paramMap.get('evaluationId') ?? '';
  const target: DeniedTarget = { kind: 'evaluation', name: id, back: `/project/${project}` };
  return inject(EvaluationsService).getEvaluation(id).pipe(
    switchMap((evaluation) => evaluation.task_name
      ? tasks.getTask(project, evaluation.task_name).pipe(map((task) => task.id === evaluation.task))
      : of(true)),
    map((inProject) => inProject || deniedPage(router, target, state.url)),
    denyOnRefusal(target, state.url),
  );
};

export const execJobInTaskGuard: CanActivateFn = (route, state) => {
  const router = inject(Router);
  const project = route.paramMap.get('project') ?? '';
  const task = route.paramMap.get('task') ?? '';
  const id = route.paramMap.get('job') ?? '';
  const target: DeniedTarget = { kind: 'execJob', name: id, back: `/project/${project}/task/${task}` };
  return inject(ExecJobsService).get(id).pipe(
    map((job) => (job.project === project && job.task === task) || deniedPage(router, target, state.url)),
    denyOnRefusal(target, state.url),
  );
};

export const workerInProjectGuard: CanActivateFn = (route, state) => {
  const router = inject(Router);
  const project = route.paramMap.get('project') ?? '';
  const id = route.paramMap.get('workerId') ?? '';
  const target: DeniedTarget = { kind: 'worker', name: id, back: `/project/${project}/workers` };
  return inject(WorkersService).getWorkers(project).pipe(
    map((workers) => workers.some((worker) => worker.worker_id === id) || deniedPage(router, target, state.url)),
    denyOnRefusal(target, state.url),
  );
};
