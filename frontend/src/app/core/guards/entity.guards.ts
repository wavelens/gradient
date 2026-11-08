/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { inject } from '@angular/core';
import { CanActivateFn, Router } from '@angular/router';
import { Observable, map } from 'rxjs';
import { BoardService } from '@core/services/board.service';
import { EvaluationsService } from '@core/services/evaluations.service';
import { ExecJobsService } from '@core/services/exec-jobs.service';
import { WorkersService } from '@core/services/workers.service';
import { DeniedTarget, deniedPage, denyOnRefusal } from '@core/routing/access-denied';
import { links } from '@shared/routing';

export const execJobInTaskGuard: CanActivateFn = (route, state) => {
  const router = inject(Router);
  const project = route.paramMap.get('project') ?? '';
  const task = route.paramMap.get('task') ?? '';
  const id = route.paramMap.get('job') ?? '';
  const target: DeniedTarget = { kind: 'execJob', name: id, back: String(links.task(project, task)) };
  return inject(ExecJobsService).get(id).pipe(
    map((job) => (job.project === project && job.task === task) || deniedPage(router, target, state.url)),
    denyOnRefusal(target, state.url),
  );
};

export const buildOrEvaluationAccessGuard: CanActivateFn = (route, state) => {
  const evaluations = inject(EvaluationsService);
  const project = route.paramMap.get('project') ?? '';
  const id = route.paramMap.get('buildId') ?? route.paramMap.get('id') ?? '';
  const isEvaluation = route.paramMap.get('kind') === 'eval';
  const target: DeniedTarget = { kind: isEvaluation ? 'evaluation' : 'build', name: id, back: String(links.project(project)) };
  const lookup: Observable<unknown> = isEvaluation ? evaluations.getEvaluation(id) : evaluations.getBuild(id);
  return lookup.pipe(map(() => true as const), denyOnRefusal(target, state.url));
};

export const jobAssignedOrPendingGuard: CanActivateFn = (route, state) => {
  const router = inject(Router);
  const board = inject(BoardService);
  const id = route.paramMap.get('id') ?? '';
  const target: DeniedTarget = { kind: 'job', name: id, back: String(links.board()) };
  if (route.data['pending'] !== true) {
    return board.getJob(id).pipe(map(() => true as const), denyOnRefusal(target, state.url));
  }
  return board.getPendingJobs().pipe(
    map(({ jobs }) => jobs.some((p) => p.exec_job?.id === id || (!p.exec_job && (p.evaluation_id === id || p.build_id === id))) || deniedPage(router, target, state.url)),
    denyOnRefusal(target, state.url),
  );
};

export const clusterJobBoardGuard: CanActivateFn = (route, state) => {
  const id = route.paramMap.get('id') ?? '';
  return inject(BoardService).getClusterJob(id).pipe(
    map(() => true as const),
    denyOnRefusal({ kind: 'clusterJob', name: id, back: String(links.board()) }, state.url),
  );
};

export const workerInProjectGuard: CanActivateFn = (route, state) => {
  const router = inject(Router);
  const project = route.paramMap.get('project') ?? '';
  const id = route.paramMap.get('workerId') ?? '';
  const target: DeniedTarget = { kind: 'worker', name: id, back: String(links.projectPage(project, 'workers')) };
  return inject(WorkersService).getWorkers(project).pipe(
    map((workers) => workers.some((worker) => worker.worker_id === id) || deniedPage(router, target, state.url)),
    denyOnRefusal(target, state.url),
  );
};

export const entryPointEvalQueryParameterMissingGuard: CanActivateFn = (route) =>
  !!route.queryParamMap.get('eval') || links.taskPage(route.paramMap.get('project') ?? '', route.paramMap.get('task') ?? '', 'metrics');
