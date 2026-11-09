/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { inject } from '@angular/core';
import { CanActivateFn, Router } from '@angular/router';
import { Observable, map, of, switchMap } from 'rxjs';
import { MessageService } from '@gradient/ui/ui';
import { EvaluationResponse } from '@core/models';
import { EvaluationsService } from '@core/services/evaluations.service';
import { ExecJobsService } from '@core/services/exec-jobs.service';
import { WorkersService } from '@core/services/workers.service';
import { DeniedTarget, deniedPage, denyOnRefusal, notFoundPageRedirect } from '@core/routing/access-denied';
import { redirectToCorrectProjectSlug } from '@core/resolvers/evaluation-access.resolver';
import { links } from '@shared/routing';

export const execJobInTaskGuard: CanActivateFn = (route, state) => {
  const project = route.paramMap.get('project') ?? '';
  const task = route.paramMap.get('task') ?? '';
  const id = route.paramMap.get('job') ?? '';
  const target: DeniedTarget = { kind: 'execJob', back: String(links.projects()) };
  const notFoundPage = notFoundPageRedirect(target, state.url, project);
  return inject(ExecJobsService).get(id).pipe(
    switchMap((job) => (job.project === project && job.task === task ? of(true) : notFoundPage())),
    denyOnRefusal(target, state.url, project),
  );
};

export const buildOrEvaluationAccessGuard: CanActivateFn = (route, state) => {
  const router = inject(Router);
  const evaluations = inject(EvaluationsService);
  const project = route.paramMap.get('project') ?? '';
  const id = route.paramMap.get('buildId') ?? route.paramMap.get('id') ?? '';
  const isEvaluation = route.paramMap.get('kind') === 'eval';
  const target: DeniedTarget = { kind: isEvaluation ? 'evaluation' : 'build', back: String(links.projects()) };
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

export const entryPointEvalQueryParameterMissingGuard: CanActivateFn = (route) => {
  if (route.queryParamMap.get('eval')) return true;
  inject(MessageService).add({ severity: 'info', summary: 'Select an entry point to view metrics for that entry point.' });
  return links.taskPage(route.paramMap.get('project') ?? '', route.paramMap.get('task') ?? '', 'metrics');
};
