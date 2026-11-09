/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { inject } from '@angular/core';
import { CanActivateFn, Router } from '@angular/router';
import { Observable, map, switchMap } from 'rxjs';
import { EvaluationResponse } from '@core/models';
import { EvaluationsService } from '@core/services/evaluations.service';
import { ExecJobsService } from '@core/services/exec-jobs.service';
import { WorkersService } from '@core/services/workers.service';
import { DeniedTarget, deniedPage, denyOnRefusal } from '@core/routing/access-denied';
import { redirectToCorrectProjectSlug } from '@core/resolvers/evaluation-access.resolver';
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
  const router = inject(Router);
  const evaluations = inject(EvaluationsService);
  const project = route.paramMap.get('project') ?? '';
  const id = route.paramMap.get('buildId') ?? route.paramMap.get('id') ?? '';
  const isEvaluation = route.paramMap.get('kind') === 'eval';
  const target: DeniedTarget = { kind: isEvaluation ? 'evaluation' : 'build', name: id, back: String(links.projects()) };
  const evaluation$: Observable<EvaluationResponse> = isEvaluation
    ? evaluations.getEvaluation(id)
    : evaluations.getBuild(id).pipe(switchMap((build) => evaluations.getEvaluation(build.evaluation)));
  return evaluation$.pipe(
    map((evaluation) => evaluation.project_name === project || redirectToCorrectProjectSlug(router, state.url, evaluation.project_name)),
    denyOnRefusal(target, state.url, project),
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
