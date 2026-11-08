/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import {
  ActivatedRouteSnapshot,
  CanActivateFn,
  RedirectCommand,
  Router,
  RouterStateSnapshot,
  convertToParamMap,
  provideRouter,
} from '@angular/router';
import { Observable, firstValueFrom, isObservable, of, throwError } from 'rxjs';
import { ApiError } from '@core/services/api.service';
import { AuthService } from '@core/services/auth.service';
import { EvaluationsService } from '@core/services/evaluations.service';
import { ExecJobsService } from '@core/services/exec-jobs.service';
import { ProjectsService } from '@core/services/projects.service';
import { TasksService } from '@core/services/tasks.service';
import { TeamsService } from '@core/services/teams.service';
import { WorkersService } from '@core/services/workers.service';
import { taskAccessResolver } from '@core/resolvers/task-access.resolver';
import { cacheAccessResolver } from '@core/resolvers/cache-access.resolver';
import { evaluationAccessResolver } from '@core/resolvers/evaluation-access.resolver';
import { teamAccessResolver } from '@core/resolvers/team-access.resolver';
import { CachesService } from '@core/services/caches.service';
import { adminGuard } from './admin.guard';
import {
  buildOrEvaluationAccessGuard,
  clusterJobBoardGuard,
  execJobInTaskGuard,
  jobAssignedOrPendingGuard,
  workerInProjectGuard,
} from './entity.guards';
import { BoardService } from '@core/services/board.service';
import { visibleProjectGuard } from './visible-project.guard';

const URL = '/project/acme/somewhere';
const refused = (status: number) => throwError(() => new ApiError('refused', status));

function configure(services: Record<string, unknown>, user: { superuser: boolean } | null = { superuser: false }) {
  const fakes = {
    projects: { getProject: () => of({}) },
    tasks: { getTask: () => of({ id: 't1' }) },
    caches: { getCache: () => of({}) },
    teams: { get: () => of({}) },
    evaluations: { getEvaluation: () => of({ task: 't1', task_name: 'demo' }), getBuild: () => of({ id: 'b1' }) },
    board: { getJob: () => of({}), getPendingJobs: () => of({ jobs: [] }), getClusterJob: () => of({}) },
    execJobs: { get: () => of({ project: 'acme', task: 'demo' }) },
    workers: { getWorkers: () => of([{ worker_id: 'w1' }]) },
    ...services,
  };
  TestBed.configureTestingModule({
    providers: [
      provideRouter([]),
      { provide: ProjectsService, useValue: fakes.projects },
      { provide: TasksService, useValue: fakes.tasks },
      { provide: CachesService, useValue: fakes.caches },
      { provide: TeamsService, useValue: fakes.teams },
      { provide: EvaluationsService, useValue: fakes.evaluations },
      { provide: ExecJobsService, useValue: fakes.execJobs },
      { provide: WorkersService, useValue: fakes.workers },
      { provide: BoardService, useValue: fakes.board },
      {
        provide: AuthService,
        useValue: { initialized$: of(true), resolveSession: () => of(!!user), serverUnreachable: () => false, user: () => user, isAuthenticated: () => !!user },
      },
    ],
  });
}

async function outcome(guard: CanActivateFn, params: Record<string, string>, data: Record<string, unknown> = {}): Promise<unknown> {
  const result = TestBed.runInInjectionContext(() =>
    guard({ paramMap: convertToParamMap(params), data } as ActivatedRouteSnapshot, { url: URL } as RouterStateSnapshot),
  );
  return isObservable(result) ? firstValueFrom(result as Observable<unknown>, { defaultValue: 'cancelled' }) : result;
}

function page(result: unknown): string {
  expect(result).toBeInstanceOf(RedirectCommand);
  const command = result as RedirectCommand;
  expect(command.navigationBehaviorOptions?.browserUrl).toBe(URL);
  return TestBed.inject(Router).serializeUrl(command.redirectTo);
}

describe('not-found or no-access page', () => {
  it('replaces a project a signed-in visitor cannot see', async () => {
    configure({ projects: { getProject: () => refused(404) } });
    expect(page(await outcome(visibleProjectGuard, { project: 'acme' }))).toBe(
      `/error/404?kind=project&name=acme&back=%2Fprojects&from=${encodeURIComponent(URL)}`,
    );
  });

  it('sends an anonymous visitor of a hidden project, task or cache to sign-in and back', async () => {
    const login = `/account/login?next=${encodeURIComponent(URL)}`;
    const redirect = (result: unknown) => {
      expect(result).toBeInstanceOf(RedirectCommand);
      return TestBed.inject(Router).serializeUrl((result as RedirectCommand).redirectTo);
    };
    configure({ projects: { getProject: () => refused(404) }, tasks: { getTask: () => refused(403) }, caches: { getCache: () => refused(404) } }, null);
    expect(redirect(await outcome(visibleProjectGuard, { project: 'acme' }))).toBe(login);
    expect(redirect(await outcome(taskAccessResolver as unknown as CanActivateFn, { project: 'acme', task: 'demo' }))).toBe(login);
    expect(redirect(await outcome(cacheAccessResolver as unknown as CanActivateFn, { cache: 'store' }))).toBe(login);
  });

  it('lets a visible project through', async () => {
    configure({});
    expect(await outcome(visibleProjectGuard, { project: 'acme' })).toBe(true);
  });

  it('replaces a task without access and links back to its project', async () => {
    configure({ tasks: { getTask: () => refused(403) } });
    expect(page(await outcome(taskAccessResolver as unknown as CanActivateFn, { project: 'acme', task: 'demo' }))).toContain(
      'kind=task&name=demo&back=%2Fproject%2Facme',
    );
  });

  it('replaces a missing cache', async () => {
    configure({ caches: { getCache: () => refused(404) } });
    expect(page(await outcome(cacheAccessResolver as unknown as CanActivateFn, { cache: 'private' }))).toContain('kind=cache&name=private');
  });

  it('replaces a missing team', async () => {
    configure({ teams: { get: () => refused(404) } });
    expect(page(await outcome(teamAccessResolver as unknown as CanActivateFn, { team: 'qa' }))).toContain('kind=team&name=qa&back=%2Fteams');
  });

  it('replaces an evaluation that belongs to another project', async () => {
    configure({ tasks: { getTask: () => of({ id: 'other-task' }) } });
    expect(page(await outcome(evaluationAccessResolver as unknown as CanActivateFn, { project: 'acme', evaluationId: 'e1' }))).toContain('kind=evaluation&name=e1');
  });

  it('hands an evaluation of the project and its task to the page', async () => {
    configure({});
    expect(await outcome(evaluationAccessResolver as unknown as CanActivateFn, { project: 'acme', evaluationId: 'e1' })).toEqual({
      evaluation: { task: 't1', task_name: 'demo' },
      task: { id: 't1' },
    });
  });

  it('replaces an exec job opened under another task', async () => {
    configure({ execJobs: { get: () => of({ project: 'acme', task: 'other' }) } });
    expect(page(await outcome(execJobInTaskGuard, { project: 'acme', task: 'demo', job: 'j1' }))).toContain(
      'kind=execJob&name=j1&back=%2Fproject%2Facme%2Ftask%2Fdemo',
    );
  });

  it.each<{ path: string; params: Record<string, string>; kind: string }>([
    { path: 'artefacts', params: { project: 'acme', buildId: 'b9' }, kind: 'build' },
    { path: 'build closure', params: { project: 'acme', kind: 'build', id: 'b9' }, kind: 'build' },
    { path: 'evaluation closure', params: { project: 'acme', kind: 'eval', id: 'b9' }, kind: 'evaluation' },
  ])('replaces a missing id on the $path page', async ({ params, kind }) => {
    configure({ evaluations: { getEvaluation: () => refused(404), getBuild: () => refused(404) } });
    expect(page(await outcome(buildOrEvaluationAccessGuard, params))).toContain(`kind=${kind}&name=b9&back=%2Fproject%2Facme`);
  });

  it('Route a waiting job to the pending job page', async () => {
    configure({ board: { getPendingJobs: () => of({ jobs: [{ evaluation_id: 'e1', build_id: 'b1' }] }) } });
    expect(await outcome(jobAssignedOrPendingGuard, { id: 'b1' }, { pending: true })).toBe(true);
  });

  it('Replace a job no longer waiting on the pending jobs page', async () => {
    configure({ board: { getPendingJobs: () => of({ jobs: [] }) } });
    expect(page(await outcome(jobAssignedOrPendingGuard, { id: 'gone' }, { pending: true }))).toContain('kind=job&name=gone&back=%2Fboard%2Foverview');
  });

  it('Replace a nonexistent job on the assigned job page', async () => {
    configure({ board: { getJob: () => refused(404) } });
    expect(page(await outcome(jobAssignedOrPendingGuard, { id: 'gone' }))).toContain('kind=job&name=gone&back=%2Fboard%2Foverview');
  });

  it('replaces a missing cluster job', async () => {
    configure({ board: { getClusterJob: () => refused(404) } });
    expect(page(await outcome(clusterJobBoardGuard, { id: 'c1' }))).toContain('kind=clusterJob&name=c1');
  });

  it('replaces an unknown worker', async () => {
    configure({});
    expect(page(await outcome(workerInProjectGuard, { project: 'acme', workerId: 'nope' }))).toContain('kind=worker&name=nope');
  });

  it('leaves an unreachable server to the error page the interceptor shows', async () => {
    configure({ teams: { get: () => refused(503) } });
    expect(await outcome(teamAccessResolver as unknown as CanActivateFn, { team: 'qa' })).toBe('cancelled');
  });

  it('shows the server error page for other failures', async () => {
    configure({ teams: { get: () => refused(500) } });
    expect(page(await outcome(teamAccessResolver as unknown as CanActivateFn, { team: 'qa' }))).toContain('/error/500');
  });

  it('shows the admin-only page to a signed-in user without superuser rights', async () => {
    configure({});
    expect(page(await outcome(adminGuard, {}))).toBe(`/error/403?from=${encodeURIComponent(URL)}`);
  });

  it('lets a superuser into admin pages', async () => {
    configure({}, { superuser: true });
    expect(await outcome(adminGuard, {})).toBe(true);
  });
});
