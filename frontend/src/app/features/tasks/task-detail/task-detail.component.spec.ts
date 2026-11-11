/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { By } from '@angular/platform-browser';
import { ActivatedRoute, ParamMap, Router, convertToParamMap, provideRouter } from '@angular/router';
import { HttpHeaders, HttpResponse, provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { vi } from 'vitest';
import { BehaviorSubject, EMPTY, NEVER, type Observable, Subject, of, throwError } from 'rxjs';
import { TaskDetailComponent } from './task-detail.component';
import { ExecJobListComponent } from '@features/exec-jobs/exec-job-list/exec-job-list.component';
import { TasksService } from '@core/services/tasks.service';
import { EvaluationsService } from '@core/services/evaluations.service';
import { ProjectsService } from '@core/services/projects.service';
import { AuthService } from '@core/services/auth.service';
import { StarsService } from '@core/services/stars.service';
import { LiveEvent, LiveService } from '@core/services/live.service';
import { NotifyService } from '@core/services/notify.service';
import { ApiError } from '@core/services/api.service';
import { AccessState } from '@core/models/access.model';
import { BuildStatusCounts, EntryPointSummary, EvaluationSummary, ProjectReadinessResponse, TaskDetailsResponse } from '@core/models';
import { ConfirmService, type ConfirmRequest } from '@shared/confirm';
import { monthDayClockTime } from '@shared/text';
import { CopyMenuService } from '@shared/routing';

const projectReadinessResponseWithNoRegisteredWorker: ProjectReadinessResponse = {
  ready: false,
  workers: { ready: false, registered: 0, teams: [], teams_granted_without_workers: [], teams_you_can_grant: [] },
  cache: { ready: true, subscriptions: [] },
  repository: { status: 'verified', tasks: [] },
};

function zeroCounts(): BuildStatusCounts {
  return { completed: 0, failed: 0, building: 0, queued: 0, substituted: 0, aborted: 0, dependency_failed: 0 };
}

function epSummary(id: string, attr = `packages."x86_64-linux".${id}`): EntryPointSummary {
  return {
    id,
    build_id: `b-${id}`,
    derivation_path: `aaaa-${id}.drv`,
    eval: attr,
    build_status: 'Completed',
    has_artefacts: false,
    outputs: {},
    architecture: 'x86_64-linux',
    build_time_ms: null,
    deps: zeroCounts(),
    deps_total: 0,
    prioritized: false,
    ifd: false,
    created_at: '2026-01-01T00:00:00',
  } as EntryPointSummary;
}

function evalSummary(
  id: string,
  status: EvaluationSummary['status'] = 'Building',
  extra: Partial<EvaluationSummary> = {},
): EvaluationSummary {
  return {
    id,
    commit: 'abc1234def5678',
    commit_message: null,
    status,
    wildcard: 'packages.x86_64-linux.**',
    trigger: null,
    triggered_by: null,
    pr_number: null,
    total_builds: 0,
    builds: zeroCounts(),
    errors: 0,
    warnings: 0,
    dispatched_job: null,
    prioritized: false,
    only_evaluate: false,
    created_at: '2026-01-01T00:00:00',
    started_at: null,
    finished_at: null,
    updated_at: '2026-01-01T00:01:00',
    phase_started_at: {},
    ...extra,
  };
}

function activatedRouteStub(access: AccessState, query: BehaviorSubject<ParamMap>, task: unknown): ActivatedRoute {
  return {
    snapshot: {
      paramMap: convertToParamMap({ project: 'acme', task: 'demo' }),
    },
    queryParamMap: query,
    data: of({}),
    parent: { data: of({ taskAccess: { task, access } }) },
  } as unknown as ActivatedRoute;
}

function taskFor(
  access: AccessState,
  extraEvals: EvaluationSummary[] = [],
  primaryStatus: EvaluationSummary['status'] = 'Building',
  primary: Partial<EvaluationSummary> = {},
  repository = '',
) {
  return {
    id: 'p',
    name: 'demo',
    display_name: 'Demo',
    description: '',
    repository,
    wildcard: '*',
    active: true,
    created_at: '2026-01-01T00:00:00',
    keep_evaluations: 5,
    last_check_at: '2026-01-01T00:00:00',
    queue: { building: 0, queued: 0 },
    branches: [null] as (string | null)[],
    last_evaluations: [evalSummary('e1', primaryStatus, primary), ...extraEvals],
    can_edit: access.canEdit,
    can_trigger: access.canTrigger,
    permissions: [...(access.canEdit ? ['editTask' as const, 'manageWorkers' as const] : []), ...(access.canTrigger ? ['triggerEvaluation' as const] : [])],
    managed: access.managed,
  };
}

function findByText(root: HTMLElement, text: string): HTMLElement | null {
  const target = text.toLowerCase();
  return (Array.from(root.querySelectorAll('button')) as HTMLElement[]).find(
    (el) => (el.textContent ?? '').trim().toLowerCase().includes(target),
  ) ?? null;
}

type SetupOverrides = Partial<{
  getTask: () => ReturnType<TasksService['getTask']>;
  getProject: () => ReturnType<ProjectsService['getProject']>;
  startEvaluation: () => ReturnType<TasksService['startEvaluation']>;
  abortEvaluation: (project: string, proj: string, id: string) => ReturnType<TasksService['abortEvaluation']>;
  downloadReport: () => ReturnType<TasksService['downloadReport']>;
  getEntryPoints: () => ReturnType<TasksService['getEntryPoints']>;
  getEvaluations: TasksService['getEvaluations'];
  getEvaluation: () => unknown;
  extraEvals: EvaluationSummary[];
  primaryStatus: EvaluationSummary['status'];
  primary: Partial<EvaluationSummary>;
  repository: string;
  task: Partial<TaskDetailsResponse>;
}>;

function resolvedTask(access: AccessState, overrides: SetupOverrides) {
  return { ...taskFor(access, overrides.extraEvals ?? [], overrides.primaryStatus, overrides.primary, overrides.repository), ...overrides.task } as ReturnType<typeof taskFor>;
}

function makeTasksService(access: AccessState, overrides: SetupOverrides = {}): TasksService {
  return {
    getTask: overrides.getTask ?? (() => of(resolvedTask(access, overrides))),
    getEntryPoints: overrides.getEntryPoints ?? (() => of({ entry_points: [], total: 0, failed_attributes: [] })),
    getEvaluations: overrides.getEvaluations ?? (() => of([])),
    startEvaluation: overrides.startEvaluation ?? (() => of({ id: 'ok', name: 'acme/demo', url: '' })),
    abortEvaluation: overrides.abortEvaluation ?? (() => of('ok')),
    downloadReport: overrides.downloadReport ?? (() => NEVER),
  } as unknown as TasksService;
}

function setup(
  access: AccessState,
  serviceOverrides: Parameters<typeof makeTasksService>[1] = {},
  visitor: 'anonymous' | 'member' | 'superuser' = 'member',
  initialQuery: Record<string, string> = {},
): {
  fixture: ComponentFixture<TaskDetailComponent>;
  tasksService: TasksService;
  evaluationsService: EvaluationsService;
  query: BehaviorSubject<ParamMap>;
  frames: Subject<LiveEvent>;
  notify: { success: ReturnType<typeof vi.fn>; failure: ReturnType<typeof vi.fn> };
  confirm: ReturnType<typeof vi.fn>;
  liveConnectionChange: { fn?: (open: boolean) => void };
} {
  const query = new BehaviorSubject(convertToParamMap(initialQuery));
  const frames = new Subject<LiveEvent>();
  const liveConnectionChange: { fn?: (open: boolean) => void } = {};
  const notify = { success: vi.fn(), failure: vi.fn() };
  const confirm = vi.fn((request: ConfirmRequest): Observable<unknown> => request.action());
  const tasksService = makeTasksService(access, serviceOverrides);
  const evaluationsService = {
    getEvaluation: serviceOverrides.getEvaluation ?? (() => of({ error: null })),
    prioritizeEvaluation: () => of('Success'),
    retryEvaluation: () => of('c68e2ded-bd0c-4c4e-82fd-ec8f86f50df2'),
    prioritizeBuild: () => of('Success'),
    abortBuild: () => of('Success'),
    retryBuild: () => of('Success'),
  } as unknown as EvaluationsService;
  TestBed.configureTestingModule({
    imports: [TaskDetailComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      { provide: ActivatedRoute, useValue: activatedRouteStub(access, query, resolvedTask(access, serviceOverrides)) },
      { provide: TasksService, useValue: tasksService },
      { provide: EvaluationsService, useValue: evaluationsService },
      { provide: ProjectsService, useValue: {
        getProject: serviceOverrides.getProject ?? (() => of({ display_name: 'Acme' })),
        getProjectReadiness: () => of(projectReadinessResponseWithNoRegisteredWorker),
      } },
      { provide: AuthService, useValue: {
        isAuthenticated: () => visitor !== 'anonymous',
        user: () => (visitor === 'anonymous' ? null : { superuser: visitor === 'superuser' }),
      } },
      { provide: StarsService, useValue: { starred: vi.fn(() => of(true)), set: () => of(true) } },
      { provide: LiveService, useValue: { connect: (_path: string, onChange?: (open: boolean) => void) => {
        liveConnectionChange.fn = onChange;
        return frames;
      } } },
      { provide: NotifyService, useValue: notify },
      { provide: ConfirmService, useValue: { confirm } },
    ],
  });
  const fixture = TestBed.createComponent(TaskDetailComponent);
  fixture.detectChanges();
  return { fixture, tasksService, evaluationsService, query, frames, notify, confirm, liveConnectionChange };
}

const failedBuilds = { builds: { ...zeroCounts(), failed: 2 } };
const menuLabels = (fixture: ComponentFixture<TaskDetailComponent>) =>
  fixture.componentInstance.evalMenuModel().map(i => i.label);

describe('TaskDetailComponent - failed evaluation without packages', () => {
  const access = { managed: false, canEdit: true, canTrigger: true };

  it('shows the first line of the evaluation error instead of "No packages"', () => {
    const { fixture } = setup(access, {
      primaryStatus: 'Failed',
      getEvaluation: () => of({ error: "\nerror: in pure evaluation mode, 'fetchTree' doesn't fetch unlocked input\n  at flake.lock" }),
    });
    fixture.detectChanges();
    const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
    expect(text).toContain('Evaluation failed');
    expect(text).toContain("error: in pure evaluation mode, 'fetchTree' doesn't fetch unlocked input");
    expect(text).not.toContain('at flake.lock');
    expect(text).not.toContain('No packages');
  });

  it('still says the evaluation failed when its error cannot load', () => {
    const { fixture } = setup(access, { primaryStatus: 'Failed', getEvaluation: () => throwError(() => new Error('offline')) });
    fixture.detectChanges();
    const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
    expect(text).toContain('Evaluation failed');
    expect(text).not.toContain('No packages');
  });

  it('says an evaluation aborted before it started was never evaluated, without a tally or timer', () => {
    const { fixture } = setup(access, { primaryStatus: 'Aborted' });
    const root = fixture.nativeElement as HTMLElement;
    const text = root.textContent ?? '';
    expect(text).toContain('Aborted before the evaluation started');
    expect(text).not.toContain('No packages');
    expect(text).not.toContain('0 / 0');
    expect(root.querySelector('.panel-head .dur')).toBeNull();
    expect(root.querySelector('.panel-head .sub')?.textContent).toMatch(/queued\s+Jan 1/);
  });

  it('dates the header by the start time once the evaluation started', () => {
    const { fixture } = setup(access, { primaryStatus: 'Completed', primary: { started_at: '2026-01-02T03:04:00' } });
    const sub = (fixture.nativeElement as HTMLElement).querySelector('.panel-head .sub')?.textContent ?? '';
    expect(sub).toMatch(/started\s+Jan 2/);
    expect(sub).not.toContain('queued');
  });
});

describe('TaskDetailComponent - older evaluations', () => {
  const access = { managed: false, canEdit: true, canTrigger: true };
  const page = (prefix: string, n: number) => Array.from({ length: n }, (_, i) => evalSummary(`${prefix}${i}`, 'Queued'));

  it('appends the page before the oldest card and stops after a short page', () => {
    const getEvaluations = vi.fn()
      .mockReturnValueOnce(of(page('a', 10)))
      .mockReturnValueOnce(of(page('b', 3)));
    const { fixture } = setup(access, { extraEvals: [evalSummary('e2', 'Queued')], getEvaluations });
    const cmp = fixture.componentInstance;
    const more = () => (fixture.nativeElement as HTMLElement).querySelector('.strip-more');
    expect(more()).not.toBeNull();

    cmp.loadOlderEvaluations();
    expect(getEvaluations).toHaveBeenLastCalledWith('acme', 'demo', 10, 'e2');
    cmp.loadOlderEvaluations();
    expect(getEvaluations).toHaveBeenLastCalledWith('acme', 'demo', 10, 'a9');
    cmp.loadOlderEvaluations();
    fixture.detectChanges();

    expect(getEvaluations).toHaveBeenCalledTimes(2);
    expect(cmp.evaluations().map((e) => e.id)).toEqual(['e1', 'e2', ...page('a', 10).map((e) => e.id), 'b0', 'b1', 'b2']);
    expect(more()).toBeNull();
  });

  it('shows a failed older page inline with a retry instead of ending the strip silently', () => {
    const getEvaluations = vi.fn()
      .mockReturnValueOnce(throwError(() => new ApiError('database is down', 500)))
      .mockReturnValueOnce(of(page('a', 3)));
    const { fixture } = setup(access, { getEvaluations });
    const root = fixture.nativeElement as HTMLElement;
    fixture.componentInstance.loadOlderEvaluations();
    fixture.detectChanges();
    expect(root.querySelector('.strip-load-error')?.textContent).toContain("Couldn't load older evaluations: database is down");
    expect(root.querySelector('.strip-more')).toBeNull();

    root.querySelector<HTMLButtonElement>('.strip-load-error button')!.click();
    fixture.detectChanges();
    expect(root.querySelector('.strip-load-error')).toBeNull();
    expect(fixture.componentInstance.evaluations().map((e) => e.id)).toEqual(['e1', 'a0', 'a1', 'a2']);
  });

  it('shows the commit message and the short hash in the tooltip of an evaluation tab', () => {
    const { fixture } = setup(access, { primary: { commit_message: 'bump nonce', commit: 'abcdef1234567' } });
    expect((fixture.nativeElement as HTMLElement).querySelector('.eval-card .ev-title')?.getAttribute('title')).toBe('bump nonce · abcdef12');
  });

  it('lists an evaluation once when a refresh moves it into the newest page', () => {
    const getEvaluations = vi.fn(() => of(page('a', 10)));
    const { fixture } = setup(access, { getEvaluations });
    const cmp = fixture.componentInstance;
    cmp.loadOlderEvaluations();
    cmp.task.set({ ...cmp.task()!, last_evaluations: [evalSummary('new'), evalSummary('e1'), evalSummary('a0', 'Queued')] });

    const ids = cmp.evaluations().map((e) => e.id);
    expect(ids).toEqual(['new', 'e1', 'a0', ...page('a', 10).slice(1).map((e) => e.id)]);
  });
});

describe('TaskDetailComponent - loading', () => {
  const access = { managed: false, canEdit: true, canTrigger: true };

  it('renders the resolved task at once without loading it a second time', () => {
    const getTask = vi.fn(() => NEVER);
    const { fixture } = setup(access, { getTask });
    const root = fixture.nativeElement as HTMLElement;
    expect(getTask).not.toHaveBeenCalled();
    expect(root.querySelector('gr-loading-spinner')).toBeNull();
    expect(root.querySelector('h1')?.textContent).toContain('Demo');
    expect(root.querySelector('.eval-card')).not.toBeNull();
  });

  it('reloads the task after the live socket reconnects and shows only the reconnecting state', () => {
    const getTask = vi.fn(() => of(resolvedTask(access, {})));
    const { fixture, liveConnectionChange } = setup(access, { getTask });
    const root = fixture.nativeElement as HTMLElement;
    liveConnectionChange.fn?.(true);
    fixture.detectChanges();
    expect(root.querySelector('gr-live-status')).toBeNull();
    liveConnectionChange.fn?.(false);
    fixture.detectChanges();
    expect(root.querySelector('gr-live-status')?.textContent).toContain('Reconnecting');
    const callsBeforeReconnect = getTask.mock.calls.length;
    liveConnectionChange.fn?.(true);
    fixture.detectChanges();
    expect(getTask).toHaveBeenCalledTimes(callsBeforeReconnect + 1);
    expect(root.querySelector('gr-live-status')).toBeNull();
  });

  it('marks a paused task as inactive with a link to its settings in place of the live badge', () => {
    const { fixture, liveConnectionChange } = setup(access, { getTask: () => of({ ...resolvedTask(access, {}), active: false }) });
    liveConnectionChange.fn?.(true);
    liveConnectionChange.fn?.(false);
    liveConnectionChange.fn?.(true);
    fixture.detectChanges();
    const root = fixture.nativeElement as HTMLElement;
    expect(root.querySelector('gr-live-status')).toBeNull();
    const badge = root.querySelector<HTMLAnchorElement>('a.inactive-badge');
    expect(badge?.textContent).toContain('Inactive');
    expect(badge?.getAttribute('href')).toBe('/project/acme/task/demo/settings');
    expect(findByText(root, 'Run')?.hasAttribute('disabled')).toBe(false);
  });

  it('names the branches of the task so two tasks on one repository can be told apart', () => {
    const { fixture } = setup(access, { task: { branches: [null, 'release'] } });
    const chips = (fixture.nativeElement as HTMLElement).querySelector('.task-info')?.textContent ?? '';
    expect(chips).toContain('Default branch');
    expect(chips).toContain('release');
  });

  it('shows the next poll only while no evaluation runs', () => {
    const task = { next_polling_trigger_check_at: new Date(Date.now() + 270_000).toISOString().slice(0, 19) };
    const idle = setup(access, { primaryStatus: 'Completed', task }).fixture.nativeElement as HTMLElement;
    expect(idle.querySelector('.task-info')?.textContent).toContain('Next poll in 5m');
    TestBed.resetTestingModule();
    const running = setup(access, { primaryStatus: 'Building', task }).fixture.nativeElement as HTMLElement;
    expect(running.querySelector('.task-info')?.textContent).not.toContain('Next poll');
  });

  it('leaves the build queue to the evaluation tabs instead of repeating it in the header', () => {
    const { fixture } = setup(access, { getTask: () => of({ ...resolvedTask(access, {}), queue: { building: 1, queued: 0 } }) });
    expect((fixture.nativeElement as HTMLElement).querySelector('.task-info')?.textContent).not.toContain('building');
  });

  it('holds the project crumb with a placeholder until the project name arrives', () => {
    const { fixture } = setup(access, { getProject: () => NEVER });
    expect((fixture.nativeElement as HTMLElement).querySelector('.breadcrumb-link gr-skeleton')).not.toBeNull();
  });

  it('leads from the project list to the task by display name', () => {
    const { fixture } = setup(access);
    const crumbs = Array.from((fixture.nativeElement as HTMLElement).querySelectorAll('.breadcrumb-link, .breadcrumb-current'));
    expect(crumbs.map((crumb) => crumb.textContent?.trim())).toEqual(['Projects', 'Acme', 'Demo']);
    expect(crumbs.map((crumb) => crumb.getAttribute('href'))).toEqual(['/projects', '/project/acme', null]);
  });

  it('leaves the packages empty under their heading until the entry points arrive', () => {
    const { fixture } = setup(access, { primaryStatus: 'Completed', getEntryPoints: () => NEVER });
    const root = fixture.nativeElement as HTMLElement;
    expect(root.querySelector('.pkg-label')?.textContent).toContain('Packages');
    for (const sel of ['gr-loading-spinner', '.pkg', 'gr-empty-state']) {
      expect(root.querySelector(sel)).toBeNull();
    }
  });
});

describe('TaskDetailComponent - access gating', () => {
  it('hides Run / Retry failed builds / Abort when canTrigger is false', () => {
    const { fixture } = setup(
      { managed: false, canEdit: false, canTrigger: false },
      { primaryStatus: 'Completed', primary: failedBuilds },
    );
    expect(findByText(fixture.nativeElement, 'run')).toBeNull();
    expect(menuLabels(fixture)).not.toContain('Retry failed builds');
    expect(findByText(fixture.nativeElement, 'abort')).toBeNull();
  });

  it('keeps Run / Retry failed builds enabled on state-managed tasks', () => {
    const { fixture } = setup(
      { managed: true, canEdit: true, canTrigger: true },
      { primaryStatus: 'Completed', primary: failedBuilds },
    );
    const startBtn = findByText(fixture.nativeElement, 'run') as HTMLButtonElement | null;
    expect(startBtn).not.toBeNull();
    expect(startBtn!.disabled).toBe(false);
    expect(menuLabels(fixture)).toContain('Retry failed builds');
  });

  it('shows Run / Retry failed builds to a caller with TriggerEvaluation but not EditTask', () => {
    const { fixture } = setup(
      { managed: false, canEdit: false, canTrigger: true },
      { primaryStatus: 'Completed', primary: failedBuilds },
    );
    const startBtn = findByText(fixture.nativeElement, 'run') as HTMLButtonElement | null;
    expect(startBtn).not.toBeNull();
    expect(startBtn!.disabled).toBe(false);
    expect(menuLabels(fixture)).toContain('Retry failed builds');
  });
});

describe('TaskDetailComponent evaluation menu', () => {
  const trigger = { managed: false, canEdit: true, canTrigger: true };

  const retryItem = (fixture: ComponentFixture<TaskDetailComponent>) =>
    fixture.componentInstance.evalMenuModel().find(i => i.label === 'Retry failed builds');

  const panelActions = (fixture: ComponentFixture<TaskDetailComponent>) =>
    Array.from((fixture.nativeElement as HTMLElement).querySelectorAll<HTMLElement>('.panel-actions > a, .panel-actions > button'));
  const actionLabels = (fixture: ComponentFixture<TaskDetailComponent>) =>
    panelActions(fixture).map((el) => el.getAttribute('aria-label') ?? el.querySelector('.gr-button__label')?.textContent?.trim());

  it('puts logs, summary and abort before the dots of a running evaluation', () => {
    const { fixture } = setup(trigger, { primaryStatus: 'Building' });
    expect(actionLabels(fixture)).toEqual(['Logs', 'Summary', 'Abort', 'More']);
    expect(panelActions(fixture)[0].getAttribute('href')).toBe('/project/acme/log/e1');
    expect(panelActions(fixture)[1].getAttribute('href')).toBe('/project/acme/summary/e1');
  });

  it('shows retry in place of abort once a failed evaluation stopped and runs it', () => {
    const { fixture, evaluationsService } = setup(trigger, { primaryStatus: 'Completed', primary: failedBuilds });
    const spy = vi.spyOn(evaluationsService, 'retryEvaluation');
    expect(actionLabels(fixture)).toEqual(['Logs', 'Summary', 'Retry failed builds', 'More']);
    panelActions(fixture)[2].click();
    expect(spy).toHaveBeenCalledWith('e1');
  });

  it('keeps logs, summary and retry out of the dots menu', () => {
    const { fixture } = setup(trigger, { primaryStatus: 'Completed', primary: failedBuilds });
    const labels = fixture.componentInstance.panelMenuModel().map(i => i.label);
    for (const label of ['Logs', 'Summary', 'Retry failed builds']) expect(labels).not.toContain(label);
  });

  it('retries the selected evaluation from the menu, then refreshes', () => {
    const { fixture, tasksService, evaluationsService } = setup(trigger, { primaryStatus: 'Completed', primary: failedBuilds });
    const spy = vi.spyOn(evaluationsService, 'retryEvaluation');
    const reload = vi.spyOn(tasksService, 'getTask');
    retryItem(fixture)!.command!();
    expect(spy).toHaveBeenCalledWith('e1');
    expect(reload).toHaveBeenCalled();
  });

  it('retries an evaluation whose builds were only aborted', () => {
    const { fixture } = setup(trigger, { primaryStatus: 'Aborted', primary: { builds: { ...zeroCounts(), aborted: 1 } } });
    expect(retryItem(fixture)?.disabled).toBe(false);
  });

  it('offers no retry while the evaluation is still active', () => {
    const { fixture } = setup(trigger, { primaryStatus: 'Building', primary: failedBuilds });
    expect(menuLabels(fixture)).not.toContain('Retry failed builds');
  });

  it('offers no retry when nothing failed', () => {
    const { fixture } = setup(trigger, { primaryStatus: 'Completed' });
    expect(menuLabels(fixture)).not.toContain('Retry failed builds');
  });

  it('offers no retry to an anonymous visitor', () => {
    const { fixture } = setup(trigger, { primaryStatus: 'Completed', primary: failedBuilds }, 'anonymous');
    expect(menuLabels(fixture)).not.toContain('Retry failed builds');
  });

  it('retries an older finished evaluation by its id', () => {
    const { fixture, evaluationsService } = setup(trigger, {
      primaryStatus: 'Completed',
      extraEvals: [evalSummary('e0', 'Failed', failedBuilds)],
    });
    const spy = vi.spyOn(evaluationsService, 'retryEvaluation');
    fixture.componentInstance.select(fixture.componentInstance.evaluations()[1]);
    retryItem(fixture)!.command!();
    expect(spy).toHaveBeenCalledWith('e0');
  });

  it('holds the retry while a newer evaluation is in progress', () => {
    const { fixture } = setup(trigger, {
      primaryStatus: 'Building',
      extraEvals: [evalSummary('e0', 'Failed', failedBuilds)],
    });
    fixture.componentInstance.select(fixture.componentInstance.evaluations()[1]);
    expect(retryItem(fixture)?.disabledReason).toBe('An evaluation of this task is in progress.');
  });

  it('reports a failed retry', () => {
    const { fixture, evaluationsService, notify } = setup(trigger, { primaryStatus: 'Completed', primary: failedBuilds });
    const error = new ApiError('Evaluation is still running', 409);
    vi.spyOn(evaluationsService, 'retryEvaluation').mockReturnValue(throwError(() => error));
    retryItem(fixture)!.command!();
    expect(notify.failure).toHaveBeenCalledWith('Failed to retry evaluation', error);
  });

  describe('Run dialog', () => {
    const radioLabels = () =>
      Array.from(document.querySelectorAll<HTMLInputElement>('input[name="run-scope"]'))
        .map((input) => ({ value: input.value, disabled: input.disabled, text: input.parentElement?.textContent?.replace(/\s+/g, ' ').trim() }));

    it('opens from the header Run button with the latest commit chosen and no input walk', () => {
      const { fixture } = setup(trigger, { primaryStatus: 'Completed' });
      const comp = fixture.componentInstance;
      comp.runScopeChoice.set('this-commit');
      comp.walkAllInputsChecked.set(true);
      findByText(fixture.nativeElement, 'run')!.click();
      fixture.detectChanges();
      expect(comp.runDialogOpen()).toBe(true);
      expect(comp.runScopeChoice()).toBe('latest-commit');
      expect(comp.walkAllInputsChecked()).toBe(false);
      expect(radioLabels()).toEqual([
        { value: 'latest-commit', disabled: false, text: 'Latest commit on the default branch' },
        { value: 'this-commit', disabled: false, text: 'This commit abc1234d' },
      ]);
    });

    it('names the only branch of the task in the latest commit choice', () => {
      const { fixture } = setup(trigger, { task: { branches: ['main'] } });
      expect(fixture.componentInstance.runDialogStartBranchLabel()).toBe('Latest commit on main');
    });

    it('offers failed builds only for a finished evaluation with failed builds', () => {
      const { fixture } = setup(trigger, { primaryStatus: 'Failed', primary: failedBuilds });
      fixture.componentInstance.openRunDialogWithDefaultChoices();
      fixture.detectChanges();
      expect(radioLabels().map((radio) => radio.value)).toEqual(['latest-commit', 'this-commit', 'failed-builds']);
    });

    it('starts the latest commit, with a full input walk when ticked and opens the new evaluation', () => {
      const { fixture, tasksService } = setup(trigger, { primaryStatus: 'Completed' });
      const start = vi.spyOn(tasksService, 'startEvaluation').mockReturnValue(of({ id: 'e2', name: 'acme/demo', url: '' }));
      const comp = fixture.componentInstance;
      comp.openRunDialogWithDefaultChoices();
      comp.walkAllInputsChecked.set(true);
      comp.startEvaluationOrRetryFailedBuildsFromRunDialog();
      expect(start).toHaveBeenCalledWith('acme', 'demo', { walk: 'full' });
      expect(comp.runDialogOpen()).toBe(false);
      expect(comp.selectedId()).toBe('e2');
    });

    it('starts the commit of the selected evaluation', () => {
      const { fixture, tasksService } = setup(trigger, { primaryStatus: 'Completed' });
      const start = vi.spyOn(tasksService, 'startEvaluation');
      const comp = fixture.componentInstance;
      comp.openRunDialogWithDefaultChoices();
      comp.runScopeChoice.set('this-commit');
      comp.startEvaluationOrRetryFailedBuildsFromRunDialog();
      expect(start).toHaveBeenCalledWith('acme', 'demo', { commit: 'abc1234def5678' });
    });

    it('retries only the failed builds of the selected evaluation', () => {
      const { fixture, tasksService, evaluationsService } = setup(trigger, { primaryStatus: 'Failed', primary: failedBuilds });
      const start = vi.spyOn(tasksService, 'startEvaluation');
      const retry = vi.spyOn(evaluationsService, 'retryEvaluation').mockReturnValue(of('e3'));
      const comp = fixture.componentInstance;
      comp.openRunDialogWithDefaultChoices();
      comp.runScopeChoice.set('failed-builds');
      comp.startEvaluationOrRetryFailedBuildsFromRunDialog();
      expect(retry).toHaveBeenCalledWith('e1');
      expect(start).not.toHaveBeenCalled();
      expect(comp.selectedId()).toBe('e3');
    });

    it('keeps the dialog open with the server reason when the start is refused', () => {
      const { fixture, notify } = setup(trigger, {
        primaryStatus: 'Completed',
        startEvaluation: () => throwError(() => new ApiError('Task is inactive', 409)),
      });
      const comp = fixture.componentInstance;
      comp.openRunDialogWithDefaultChoices();
      comp.startEvaluationOrRetryFailedBuildsFromRunDialog();
      expect(comp.runDialogOpen()).toBe(true);
      expect(comp.runDialogStartRequestInFlight()).toBe(false);
      expect(comp.runStartFailureReason()).toBe("Couldn't start evaluation: Task is inactive");
      expect(notify.failure).not.toHaveBeenCalled();
    });
  });

  it('runs the commit of the selected evaluation again from both menus', () => {
    const { fixture, tasksService } = setup(trigger, { primaryStatus: 'Completed' });
    const start = vi.spyOn(tasksService, 'startEvaluation');
    expect(fixture.componentInstance.panelMenuModel().map(i => i.label)).toContain('Run again');
    fixture.componentInstance.evalMenuModel().find(i => i.label === 'Run again')!.command!();
    expect(start).toHaveBeenCalledWith('acme', 'demo', { commit: 'abc1234def5678' });
  });

  describe('selecting the started evaluation', () => {
    function startThenArrive(startEvaluation: () => ReturnType<TasksService['startEvaluation']>) {
      const extraEvals = [evalSummary('e0', 'Completed')];
      const { fixture, tasksService } = setup(trigger, { primaryStatus: 'Completed', extraEvals, startEvaluation });
      const evals = [evalSummary('e2', 'Queued'), ...fixture.componentInstance.evaluations()];
      vi.spyOn(tasksService, 'getTask').mockImplementation(() => of({ ...taskFor(trigger), last_evaluations: evals }));
      const entryPoints = vi.spyOn(tasksService, 'getEntryPoints').mockReturnValue(of({ entry_points: [], total: 0, failed_attributes: [] }));
      const navigate = vi.spyOn(TestBed.inject(Router), 'navigate').mockResolvedValue(true);
      const comp = fixture.componentInstance;
      comp.startEvaluation();
      return { comp, entryPoints, navigate };
    }

    it('selects the evaluation the start answers with and keeps it in the URL', () => {
      const { comp, navigate } = startThenArrive(() => of({ id: 'e2', name: 'acme/demo', url: '' }));
      expect(comp.selected()?.id).toBe('e2');
      expect(comp.starting()).toBe(false);
      expect(navigate).toHaveBeenCalledWith([], expect.objectContaining({ queryParams: { eval: 'e2' } }));
    });

    it('fetches the packages of the new evaluation once', () => {
      const { entryPoints } = startThenArrive(() => of({ id: 'e2', name: 'acme/demo', url: '' }));
      expect(entryPoints.mock.calls.filter((call) => call[2] === 'e2')).toHaveLength(1);
    });

    it('does not switch when the start fails', () => {
      const { comp } = startThenArrive(() => throwError(() => new Error('nope')));
      expect(comp.selected()?.id).toBe('e1');
    });

    it('starts one evaluation on a double click and stays busy until the new evaluation is loaded', () => {
      const { fixture, tasksService } = setup(trigger, { primaryStatus: 'Completed' });
      const start = vi.spyOn(tasksService, 'startEvaluation').mockReturnValue(of({ id: 'e2', name: 'acme/demo', url: '' }));
      const task = new Subject<ReturnType<typeof taskFor>>();
      vi.spyOn(tasksService, 'getTask').mockReturnValue(task as never);
      vi.spyOn(TestBed.inject(Router), 'navigate').mockResolvedValue(true);
      const comp = fixture.componentInstance;
      comp.startEvaluation();
      comp.startEvaluation();
      expect(start).toHaveBeenCalledTimes(1);
      expect(comp.starting()).toBe(true);
      task.next({ ...taskFor(trigger), last_evaluations: [evalSummary('e2', 'Waiting'), ...comp.evaluations()] });
      task.complete();
      expect(comp.starting()).toBe(false);
      expect(comp.latestEvaluationNotStarted()).toBe(true);
    });

    it('selects the evaluation a retry answers with', () => {
      const { fixture, tasksService, evaluationsService, notify } = setup(trigger, { primaryStatus: 'Failed', primary: failedBuilds });
      const evals = [evalSummary('e3', 'Queued'), ...fixture.componentInstance.evaluations()];
      vi.spyOn(tasksService, 'getTask').mockImplementation(() => of({ ...taskFor(trigger), last_evaluations: evals }));
      vi.spyOn(evaluationsService, 'retryEvaluation').mockReturnValue(of('e3'));
      fixture.componentInstance.retryEvaluationItem()?.command?.();
      expect(notify.success).toHaveBeenCalledWith('Evaluation retried');
      expect(fixture.componentInstance.selected()?.id).toBe('e3');
    });
  });

  it('holds Run again while an evaluation is queued', () => {
    const { fixture } = setup(trigger, { primaryStatus: 'Queued' });
    expect(fixture.componentInstance.panelMenuModel().find(i => i.label === 'Run again')?.disabledReason).toBe('The latest evaluation has not started yet.');
  });

  it('offers no Run again and no input walk without trigger access', () => {
    const { fixture } = setup({ managed: false, canEdit: false, canTrigger: false }, { primaryStatus: 'Completed' });
    expect(menuLabels(fixture)).not.toContain('Run again');
    expect(menuLabels(fixture)).not.toContain('Reevaluate all inputs');
  });

  it('links Show job to the evaluation job on the Job Board', () => {
    const { fixture } = setup(trigger, { primary: { dispatched_job: 'job-1' } });
    const item = fixture.componentInstance.panelMenuModel().find(i => i.label === 'Show job');
    expect(String(item?.routerLink)).toBe('/board/jobs/job-1');
    expect(item?.disabledReason).toBeNull();
  });

  it('copies the wait command, API URL and log page of the selected evaluation from both menus', () => {
    const copy = vi.spyOn(CopyMenuService.prototype, 'createCopyMenuItemWithCliCommandApiUrlAndLink');
    const { fixture } = setup(trigger);
    expect(fixture.componentInstance.panelMenuModel().find(i => i.label === 'Copy')?.submenuItems?.map(i => i.label)).toEqual(['CLI command', 'API URL', 'Link']);
    expect(menuLabels(fixture)).toContain('Copy');
    const references = copy.mock.calls.at(-1)![0];
    copy.mockRestore();
    expect(references.gradientCliCommand).toBe('gradient evaluation wait e1');
    expect(references.relativeApiEndpointPath).toBe('evals/e1');
    expect(String(references.absoluteLinkUrlTree)).toBe('/project/acme/log/e1');
  });

  it('copies the wait command, API URL and page of the task from the header', () => {
    const entries = vi.spyOn(CopyMenuService.prototype, 'getFlatCopyEntries');
    const { fixture } = setup(trigger);
    expect(fixture.componentInstance.copyMenuModel().map(i => i.label)).toEqual(['CLI command', 'API URL', 'Link']);
    const references = entries.mock.calls.map(([called]) => called).find((called) => called.relativeApiEndpointPath.startsWith('tasks/'))!;
    entries.mockRestore();
    expect(references.gradientCliCommand).toBe('gradient evaluation wait acme/demo');
    expect(references.relativeApiEndpointPath).toBe('tasks/acme/demo');
    expect(String(references.absoluteLinkUrlTree)).toBe('/project/acme/task/demo');
  });

  it('links Show job of a split evaluation to its cluster job', () => {
    const { fixture } = setup(trigger, { primary: { dispatched_job: 'job-1', cluster_job: 'cluster-1' } });
    const item = fixture.componentInstance.panelMenuModel().find(i => i.label === 'Show job');
    expect(String(item?.routerLink)).toBe('/board/clusters/cluster-1');
  });

  it.each(['Queued', 'Waiting'] as const)('links Show job of a %s evaluation to its pending job on the Job Board', (primaryStatus) => {
    const { fixture } = setup(trigger, { primaryStatus });
    const item = fixture.componentInstance.panelMenuModel().find(i => i.label === 'Show job');
    expect(item?.disabledReason).toBeNull();
    expect(String(item?.routerLink)).toMatch(/^\/board\/pending\/[^/]+$/);
  });

  it('says why Show job waits until an eval worker picked the evaluation up', () => {
    const { fixture } = setup(trigger);
    expect(fixture.componentInstance.panelMenuModel().find(i => i.label === 'Show job')?.disabledReason).toBe('No job for this evaluation on the job board.');
  });
});

describe('TaskDetailComponent - in-progress state (#452)', () => {
  it('disables Run while the latest evaluation is queued', () => {
    const { fixture } = setup({ managed: false, canEdit: true, canTrigger: true }, { primaryStatus: 'Queued' });
    const startBtn = findByText(fixture.nativeElement, 'run') as HTMLButtonElement | null;
    expect(startBtn).not.toBeNull();
    expect(startBtn!.getAttribute('aria-disabled')).toBe('true');
    expect(fixture.componentInstance.startDisabledReason()).toBe('The latest evaluation has not started yet.');
  });

  it('names the wait reason of the latest evaluation as the reason Run is blocked', () => {
    const { fixture } = setup({ managed: false, canEdit: true, canTrigger: true },
      { primaryStatus: 'Waiting', primary: { waiting_reason: { kind: 'no_cache' } } });
    const startBtn = findByText(fixture.nativeElement, 'run') as HTMLButtonElement;
    const reason = (fixture.nativeElement as HTMLElement).querySelector(`#${startBtn.getAttribute('aria-describedby')}`);
    expect(reason?.textContent).toBe('The latest evaluation has not started yet: No writable cache.');
  });

  it('enables Run while the latest evaluation runs', () => {
    const { fixture } = setup({ managed: false, canEdit: true, canTrigger: true }, { primaryStatus: 'Fetching' });
    const startBtn = findByText(fixture.nativeElement, 'run') as HTMLButtonElement | null;
    expect(startBtn!.disabled).toBe(false);
  });

  it('shows the eval status badge in the title while in progress', () => {
    const { fixture } = setup({ managed: false, canEdit: true, canTrigger: true }, { primaryStatus: 'Building' });
    const badge = fixture.nativeElement.querySelector('.title-line gr-eval-status-badge');
    expect(badge).toBeTruthy();
  });

  it('counts built, failed, skipped and aborted builds against one total in the header', () => {
    const { fixture } = setup({ managed: false, canEdit: true, canTrigger: true },
      { primaryStatus: 'Failed', primary: { builds: { ...zeroCounts(), completed: 1, failed: 1, dependency_failed: 1, aborted: 1, substituted: 2 } } });
    const counter = (fixture.nativeElement as HTMLElement).querySelector('.bar-line > span')?.textContent?.replace(/\s+/g, ' ').trim();
    expect(counter).toBe('1 of 4 built · 1 failed · 1 skipped · 1 aborted');
  });

  it('shows the time left and a filled bar from the expected duration of a running evaluation', () => {
    vi.useFakeTimers({ now: new Date('2026-01-01T00:04:00Z'), toFake: ['Date'] });
    try {
      const { fixture } = setup({ managed: false, canEdit: true, canTrigger: true }, {
        primaryStatus: 'Building',
        primary: { started_at: '2026-01-01T00:00:00', phase_started_at: { fetch: '2026-01-01T00:00:00Z' }, expected_duration_ms: 10 * 60_000 },
      });
      const progress = (fixture.nativeElement as HTMLElement).querySelector('.running-evaluation-progress-and-time-left');
      expect(progress?.textContent?.trim()).toBe('About 6 min left');
      expect(progress?.querySelector('[role="progressbar"]')?.getAttribute('aria-valuenow')).toBe('40');
    } finally {
      vi.useRealTimers();
    }
  });

  it('hides the time left once a running evaluation is past its expected duration', () => {
    const { fixture } = setup({ managed: false, canEdit: true, canTrigger: true }, {
      primaryStatus: 'Building',
      primary: { started_at: '2026-01-01T00:00:00', phase_started_at: { fetch: '2026-01-01T00:00:00Z' }, expected_duration_ms: 60_000 },
    });
    expect((fixture.nativeElement as HTMLElement).querySelector('.running-evaluation-progress-and-time-left')).toBeNull();
  });

  it('draws the shared build of an aborted evaluation as aborted, not as building stripes', () => {
    const { fixture } = setup({ managed: false, canEdit: true, canTrigger: true }, {
      primaryStatus: 'Aborted',
      primary: { builds: { ...zeroCounts(), completed: 1, building: 1 }, finished_at: '2026-01-01T00:04:00' },
      getEntryPoints: () => of({ entry_points: [{ ...epSummary('slow'), build_status: 'Building', build_started_at: '2026-01-01T00:01:00' }], total: 1, failed_attributes: [] }),
    });
    fixture.detectChanges();
    const root = fixture.nativeElement as HTMLElement;
    expect((root.querySelector('.eval-bar .seg-building') as HTMLElement).style.width).toBe('0%');
    expect(root.querySelector('.bar-line > span')?.textContent).toContain('1 aborted');
    expect(root.querySelector('.entry-row.pkg')?.getAttribute('data-phase')).toBe('aborted');
    expect((root.querySelector('.pkg-bar .seg-building') as HTMLElement).style.width).toBe('0%');
    expect(root.querySelector('.pkg-time')?.textContent?.trim()).toBe('3m 00s');
  });

  it('names the wait reason next to the badge of a building evaluation that waits for exec jobs', () => {
    const { fixture } = setup({ managed: false, canEdit: true, canTrigger: true },
      { primaryStatus: 'Building', primary: { waiting_reason: { kind: 'exec_jobs', waiting_exec_jobs: 1 } } });
    expect(fixture.nativeElement.querySelector('.title-line .waiting-reason')?.textContent).toContain('Waiting for exec jobs');
  });

  it('keeps the title badge once the evaluation finished', () => {
    const { fixture } = setup({ managed: false, canEdit: true, canTrigger: true }, { primaryStatus: 'Completed' });
    expect(fixture.nativeElement.querySelector('.title-line gr-eval-status-badge')?.textContent?.trim()).toBe('Completed');
  });
});

describe('TaskDetailComponent - evaluation progress', () => {
  it('shows activity for the selected evaluation without reloading the task', () => {
    const { fixture, tasksService, frames } = setup({ managed: false, canEdit: true, canTrigger: true }, { primaryStatus: 'EvaluatingFlake' });
    const getTask = vi.spyOn(tasksService, 'getTask');
    frames.next({ event: 'evaluation.activity', at: '', content: { evaluation_id: 'e1', progress: { kind: 'evaluating', thunks: 7 } } });
    expect(fixture.componentInstance.selectedProgress()).toEqual({ kind: 'evaluating', thunks: 7 });
    expect(getTask).not.toHaveBeenCalled();
  });

  it('lists the inputs of a fetching evaluation in the packages panel', () => {
    const inputs = [{ name: 'nixpkgs', state: 'Fetching' as const, downloaded_bytes: 10, expected_bytes: 40 }];
    const { fixture } = setup({ managed: false, canEdit: true, canTrigger: true }, {
      primaryStatus: 'Fetching',
      primary: { progress: { kind: 'fetching', inputs } },
    });
    fixture.detectChanges();
    const row = fixture.nativeElement.querySelector('.pkg[data-phase="running"]');
    expect(row?.querySelector('gr-status-icon')).toBeTruthy();
    expect(row?.textContent).toContain('nixpkgs');
  });

  it('titles the fetching inputs "Flake Inputs" and lists them by name below the downloading inputs', () => {
    const inputs = [
      { name: 'zlib', state: 'Done' as const, downloaded_bytes: 1, expected_bytes: 0 },
      { name: 'nixpkgs', state: 'Fetching' as const, downloaded_bytes: 10, expected_bytes: 40 },
      { name: 'crane', state: 'Queued' as const, downloaded_bytes: 0, expected_bytes: 0 },
    ];
    const { fixture } = setup({ managed: false, canEdit: true, canTrigger: true }, {
      primaryStatus: 'Fetching',
      primary: { progress: { kind: 'fetching', inputs } },
    });
    fixture.detectChanges();
    const panel = fixture.nativeElement.querySelector('.panel') as HTMLElement;
    expect(panel.querySelector('.pkg-label h3')?.textContent).toBe('Flake Inputs');
    expect(Array.from(panel.querySelectorAll('.pkg .pkg-name'), n => n.textContent?.trim())).toEqual(['nixpkgs', 'crane', 'zlib']);
  });

  it('shows the thunk bar instead of a spinner while evaluating', () => {
    const { fixture, frames } = setup({ managed: false, canEdit: true, canTrigger: true }, { primaryStatus: 'EvaluatingFlake' });
    frames.next({ event: 'evaluation.activity', at: '', content: { evaluation_id: 'e1', progress: { kind: 'evaluating', thunks: 1234567 } } });
    fixture.detectChanges();
    expect(fixture.nativeElement.querySelector('.panel gr-loading-spinner')).toBeNull();
    const wait = fixture.nativeElement.querySelector('.panel .pkg-wait');
    expect(wait?.textContent).toContain('Evaluating');
    expect(wait?.querySelector('gr-thunk-progress gr-segmented-bar')).toBeTruthy();
    expect(wait?.textContent).toContain('1.23M thunks');
    expect(fixture.nativeElement.querySelector('.panel .pkg')).toBeNull();
  });

  it('hides the thunk count once the evaluation has packages', () => {
    const { fixture, frames } = setup({ managed: false, canEdit: true, canTrigger: true }, {
      primaryStatus: 'EvaluatingDerivation',
      getEntryPoints: () => of({ entry_points: [epSummary('hello')], total: 1, failed_attributes: [] }),
    });
    frames.next({ event: 'evaluation.activity', at: '', content: { evaluation_id: 'e1', progress: { kind: 'evaluating', thunks: 7 } } });
    fixture.detectChanges();
    expect(fixture.nativeElement.querySelector('.pkg')).toBeTruthy();
    expect(fixture.nativeElement.querySelector('.panel').textContent).not.toContain('thunks');
  });

  it('drops the input list once the evaluation moves past Fetching', () => {
    const inputs = [{ name: 'nixpkgs', state: 'Done' as const, downloaded_bytes: 40, expected_bytes: 40 }];
    const { fixture } = setup({ managed: false, canEdit: true, canTrigger: true }, {
      primaryStatus: 'EvaluatingFlake',
      primary: { progress: { kind: 'fetching', inputs } },
    });
    fixture.detectChanges();
    expect(fixture.nativeElement.querySelector('.pkg')).toBeNull();
    expect(fixture.nativeElement.querySelector('.panel .pkg-wait')?.textContent).toContain('Waiting for packages');
  });

  it('falls back to the static text while Building, even with a late thunk frame', () => {
    const { fixture, frames } = setup({ managed: false, canEdit: true, canTrigger: true }, { primaryStatus: 'Building' });
    frames.next({ event: 'evaluation.activity', at: '', content: { evaluation_id: 'e1', progress: { kind: 'evaluating', thunks: 7 } } });
    fixture.detectChanges();
    expect(fixture.componentInstance.selectedProgress()).toBeNull();
    expect(fixture.nativeElement.querySelector('.panel .pkg-wait')?.textContent).not.toContain('thunks');
  });

  it('keeps the latest frame of each running evaluation apart', () => {
    const { fixture, frames } = setup({ managed: false, canEdit: true, canTrigger: true }, {
      primaryStatus: 'EvaluatingFlake',
      extraEvals: [evalSummary('e2', 'EvaluatingDerivation'), evalSummary('e3', 'Completed')],
    });
    const comp = fixture.componentInstance;
    frames.next({ event: 'evaluation.activity', at: '', content: { evaluation_id: 'e3', progress: { kind: 'evaluating', thunks: 1 } } });
    frames.next({ event: 'evaluation.activity', at: '', content: { evaluation_id: 'e1', progress: { kind: 'evaluating', thunks: 7 } } });
    frames.next({ event: 'evaluation.activity', at: '', content: { evaluation_id: 'e2', progress: { kind: 'evaluating', thunks: 9 } } });
    expect(comp.selectedProgress()).toEqual({ kind: 'evaluating', thunks: 7 });
    comp.selectedId.set('e2');
    expect(comp.selectedProgress()).toEqual({ kind: 'evaluating', thunks: 9 });
    expect(Object.keys((comp as unknown as { activity: () => object }).activity())).toEqual(['e1', 'e2']);
  });
});

describe('TaskDetailComponent - error surfacing (issue #280)', () => {
  it('reports a failed start with the server reason and enables the button again', () => {
    const error = new ApiError("Gradient can't read the latest commit of the repository. Check the repository URL, branch and the project's access key (deploy key), then try again.", 400, 'repository_unreachable');
    const { fixture, notify } = setup({ managed: false, canEdit: true, canTrigger: true }, {
      startEvaluation: () => throwError(() => error),
    });
    fixture.componentInstance.startEvaluation();

    expect(notify.failure).toHaveBeenCalledWith('Failed to start evaluation', error);
    expect(fixture.componentInstance.starting()).toBe(false);
  });
});

describe('TaskDetailComponent - entry point load errors', () => {
  const access = { managed: false, canEdit: true, canTrigger: true };

  it('shows a failed package load as an error, never as "No packages"', () => {
    const { fixture } = setup(access, { primaryStatus: 'Completed', getEntryPoints: () => throwError(() => new Error('database is down')) });
    fixture.detectChanges();
    const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
    expect(text).toContain('Failed to load builds');
    expect(text).toContain('database is down');
    expect(text).not.toContain('No packages');
  });

  it('loads the packages again on Try Again', () => {
    const { fixture, tasksService } = setup(access, { primaryStatus: 'Completed', getEntryPoints: () => throwError(() => new Error('down')) });
    fixture.detectChanges();
    vi.spyOn(tasksService, 'getEntryPoints').mockReturnValue(of({ entry_points: [epSummary('a')], total: 1, failed_attributes: [] }));
    (fixture.nativeElement as HTMLElement).querySelector<HTMLButtonElement>('.gr-load-state__error button')!.click();
    fixture.detectChanges();
    expect(fixture.componentInstance.entryPoints().map(e => e.id)).toEqual(['a']);
    expect((fixture.nativeElement as HTMLElement).textContent).not.toContain('Failed to load builds');
  });

  it('replaces the endless skeleton with Try Again when the next page fails', () => {
    const { fixture, tasksService } = setup(access, {
      primaryStatus: 'Completed',
      getEntryPoints: () => of({ entry_points: [epSummary('a')], total: 2, failed_attributes: [] }),
    });
    vi.spyOn(tasksService, 'getEntryPoints').mockReturnValue(throwError(() => new Error('down')));
    fixture.componentInstance.loadMoreEntryPoints();
    fixture.detectChanges();
    const root = fixture.nativeElement as HTMLElement;
    expect(root.querySelector('.pkg-more')).toBeNull();
    expect(root.textContent).toContain('Failed to load builds');
  });
});

describe('TaskDetailComponent - evaluation selection', () => {
  it('selecting an evaluation loads its entry points', () => {
    const e2 = evalSummary('e2', 'Completed');
    const { fixture, tasksService } = setup(
      { managed: false, canEdit: true, canTrigger: true },
      { extraEvals: [e2] },
    );
    const spy = vi.spyOn(tasksService, 'getEntryPoints').mockReturnValue(of({ entry_points: [], total: 0, failed_attributes: [] }));
    const component = fixture.componentInstance;
    component.select(component.evaluations()[1]);
    expect(spy).toHaveBeenCalledWith(component.projectName, component.taskName, component.evaluations()[1].id, 25, 0, '');
  });

  /// The server clamps `limit` at 500, so growing one request cannot reach past it;
  /// "show more" pages with an offset and appends what comes back.
  it('appends the next page by offset instead of growing one request', () => {
    const first = epSummary('a');
    const second = epSummary('b');
    const { fixture, tasksService } = setup(
      { managed: false, canEdit: true, canTrigger: true },
      { getEntryPoints: () => of({ entry_points: [first], total: 2, failed_attributes: [] }) },
    );
    const component = fixture.componentInstance;
    expect(component.entryPoints().map(e => e.id)).toEqual(['a']);

    const spy = vi.spyOn(tasksService, 'getEntryPoints').mockReturnValue(of({ entry_points: [second], total: 2, failed_attributes: [] }));
    component.loadMoreEntryPoints();
    const [, , , limit, offset] = spy.mock.calls.at(-1)!;

    expect(limit).toBe(25);
    expect(offset).toBe(1);
    expect(component.entryPoints().map(e => e.id)).toEqual(['a', 'b']);
    expect(component.entryPointsTotal()).toBe(2);
  });

  it('takes the status of a row past the first page from the live event', () => {
    const building = (id: string) => ({ ...epSummary(id), build_status: 'Building' as const });
    const { fixture, tasksService, frames } = setup(
      { managed: false, canEdit: true, canTrigger: true },
      { getEntryPoints: () => of({ entry_points: [building('a')], total: 2, failed_attributes: [] }) },
    );
    const component = fixture.componentInstance;
    vi.spyOn(tasksService, 'getEntryPoints').mockReturnValue(of({ entry_points: [building('b')], total: 2, failed_attributes: [] }));
    component.loadMoreEntryPoints();

    frames.next({ event: 'build.status_changed', at: '', content: { build_id: 'b-b', evaluation_id: 'e1', status: 3 } });
    expect(component.entryPoints().map(e => `${e.id}:${e.build_status}`)).toEqual(['a:Building', 'b:Completed']);
  });

  /// The row label has to be the field the server sorts by, or the list looks
  /// unordered; the derivation name it used to show is not that field.
  it('heads each attribute set and labels its rows without the set and architecture', () => {
    const { fixture } = setup(
      { managed: false, canEdit: true, canTrigger: true },
      {
        primaryStatus: 'Completed',
        getEntryPoints: () => of({
          entry_points: [epSummary('a', 'packages."x86_64-linux".hello'), epSummary('b', 'checks."x86_64-linux".fmt')],
          total: 2,
          failed_attributes: [],
        }),
      },
    );
    const root = fixture.nativeElement as HTMLElement;
    const text = (sel: string) => Array.from(root.querySelectorAll(sel)).map((e) => e.textContent?.trim());
    expect(text('.pkg-label h3')).toEqual(['Packages', 'Checks']);
    expect(text('.pkg-name')).toEqual(['hello', 'fmt']);
  });

  it('shows packages of an evaluate-only evaluation as evaluated, not as the build another task runs', () => {
    const { fixture } = setup(
      { managed: false, canEdit: true, canTrigger: true },
      {
        primaryStatus: 'Completed',
        primary: { only_evaluate: true, builds: { ...zeroCounts(), building: 1 } },
        getEntryPoints: () => of({
          entry_points: [{ ...epSummary('a'), build_status: 'Building' }],
          total: 1,
          failed_attributes: [],
        }),
      },
    );
    const root = fixture.nativeElement as HTMLElement;

    expect(root.querySelector('.entry-row.pkg')?.getAttribute('data-phase')).toBe('success');
    expect(root.querySelector('.pkg-bar')).toBeNull();
    expect(root.querySelector('.eval-bar')).toBeNull();
    expect(root.querySelector('.eval-only')?.textContent).toContain('Evaluation only');
  });

  it('shows an app row as queued while its exec job waits after the build finished', () => {
    const { fixture } = setup(
      { managed: false, canEdit: true, canTrigger: true },
      {
        primaryStatus: 'Building',
        getEntryPoints: () => of({ entry_points: [{ ...epSummary('app', 'apps.x86_64-linux.app'), build_status: 'Completed' }], total: 1, failed_attributes: [] }),
      },
    );
    const row = () => (fixture.nativeElement as HTMLElement).querySelector('.entry-row.pkg')?.getAttribute('data-phase');
    expect(row()).toBe('success');
    const execJobList = fixture.debugElement.query(By.directive(ExecJobListComponent)).componentInstance as ExecJobListComponent;
    execJobList.allExecJobsLoadedOrPolled.emit([{ id: 'x1', entry_point: 'apps.x86_64-linux.app', status: 'queued', nodes: 1, labels: [], devices: [], autostart: true, keep_vm_running: false }]);
    fixture.detectChanges();
    expect(row()).toBe('queued');
  });

  it('offers the project/task path the CLI uses for copying', () => {
    const { fixture } = setup({ managed: false, canEdit: false, canTrigger: false });
    const copy = (fixture.nativeElement as HTMLElement).querySelector('.pp-head gr-copy-field');
    expect(copy?.querySelector('code')?.textContent).toBe('acme/demo');
  });

  it('shows the own build status of an app row and links it to its exec job', () => {
    const { fixture } = setup(
      { managed: false, canEdit: true, canTrigger: true },
      {
        primaryStatus: 'Building',
        getEntryPoints: () => of({ entry_points: [{ ...epSummary('app', 'apps.x86_64-linux.app'), build_status: 'Completed' }], total: 1, failed_attributes: [] }),
      },
    );
    const execJobList = fixture.debugElement.query(By.directive(ExecJobListComponent)).componentInstance as ExecJobListComponent;
    execJobList.allExecJobsLoadedOrPolled.emit([{ id: 'x1', entry_point: 'apps.x86_64-linux.app', status: 'running', nodes: 1, labels: [], devices: [], autostart: true, keep_vm_running: false }]);
    fixture.detectChanges();
    const row = (fixture.nativeElement as HTMLElement).querySelector('.entry-row.pkg')!;
    expect(row.querySelector('.pkg-bar')).toBeNull();
    expect(row.querySelector('.pkg-deps')?.textContent?.trim()).toBe('Completed');
    expect(row.querySelector('.entry-link')?.getAttribute('href')).toBe('/project/acme/task/demo/exec/x1');
  });

  it('tags the row of a derivation an evaluation imported', () => {
    const { fixture } = setup(
      { managed: false, canEdit: true, canTrigger: true },
      {
        primaryStatus: 'Completed',
        getEntryPoints: () => of({
          entry_points: [
            epSummary('a', 'packages."x86_64-linux".hello'),
            { ...epSummary('i', 'other.x86_64-linux.source'), ifd: true },
          ],
          total: 2,
          failed_attributes: [],
        }),
      },
    );
    const root = fixture.nativeElement as HTMLElement;
    const tags = Array.from(root.querySelectorAll('.pkg-name gr-badge')).map((e) => e.textContent?.trim());
    expect(tags).toEqual(['IFD']);
  });

  it('lists an attribute that failed to evaluate as a failed row of its set', () => {
    const { fixture } = setup(
      { managed: false, canEdit: true, canTrigger: true },
      {
        primaryStatus: 'Failed',
        getEntryPoints: () => of({
          entry_points: [epSummary('a', 'nixosConfigurations.a.config.system.build.toplevel')],
          total: 1,
          failed_attributes: [{ eval: 'nixosConfigurations.b.config.system.build.toplevel', message: 'boom' }],
        }),
      },
    );
    const root = fixture.nativeElement as HTMLElement;
    const failed = root.querySelector('.pkg[data-phase="failure"]');
    expect(failed?.querySelector('.pkg-name')?.textContent?.trim()).toBe('b');
    expect(failed?.querySelector('.pkg-error')?.textContent?.trim()).toBe('Failed to evaluate');
    expect(failed?.querySelector('.entry-link')?.getAttribute('href')).toMatch(/\/log\/[^?]+\?eval=1$/);
  });

  it('shows why a waiting evaluation has no packages yet and how to fix it', () => {
    const { fixture } = setup(
      { managed: false, canEdit: true, canTrigger: true },
      {
        primaryStatus: 'Waiting',
        primary: {
          waiting_reason: {
            kind: 'workers',
            connected_workers: 0,
            available_architectures: [],
            unmet: [{ architecture: 'aarch64-linux', required_features: ['kvm'], build_count: 3 }],
          },
        },
      },
    );
    const text = (fixture.nativeElement as HTMLElement).querySelector('.pkg-wait')?.textContent ?? '';
    expect(text).toContain('Waiting for a worker');
    expect(text).toContain('This project has no connected workers.');
    expect(text).toContain('aarch64-linux with feature kvm - 3 builds');
    expect(fixture.nativeElement.querySelector('.pkg-wait a[grButton]')?.getAttribute('href')).toBe('/project/acme/workers');
  });

  it('Show attributes that failed evaluation above built rows on the first page', () => {
    const host = (n: string) => `nixosConfigurations.${n}.config.system.build.toplevel`;
    const { fixture } = setup(
      { managed: false, canEdit: true, canTrigger: true },
      {
        primaryStatus: 'Failed',
        getEntryPoints: () => of({
          entry_points: [epSummary('a', host('a'))],
          total: 2,
          failed_attributes: [{ eval: host('z'), message: 'boom' }],
        }),
      },
    );
    const names = [...(fixture.nativeElement as HTMLElement).querySelectorAll('.pkg .pkg-name')]
      .map(el => el.textContent?.trim());

    expect(names.slice(0, 2)).toEqual(['z', 'a']);
  });

  /// The server walks one dependency closure per entry point it returns, so a
  /// live poll asks for the first page and never the whole scrolled window. The
  /// rows past it are spliced back on rather than re-read.
  it('refreshes exactly one page however many rows are shown', () => {
    const { fixture, tasksService } = setup(
      { managed: false, canEdit: true, canTrigger: true },
      { getEntryPoints: () => of({ entry_points: [epSummary('a')], total: 300, failed_attributes: [] }) },
    );
    const component = fixture.componentInstance;
    expect(component.entryPoints().length).toBe(1);

    const spy = vi.spyOn(tasksService, 'getEntryPoints')
      .mockReturnValue(of({ entry_points: [epSummary('a'), epSummary('b')], total: 300, failed_attributes: [] }));
    component.loadTaskData();
    const [, , , limit, offset] = spy.mock.calls.at(-1)!;

    expect(limit).toBe(25);
    expect(offset).toBe(0);
    expect(component.entryPoints().map(e => e.id)).toEqual(['a', 'b']);
  });

  /// "show more" pages by offset, so a page that overlaps what is already shown
  /// must not repeat a row: `@for ... track ep.id` throws on a duplicate key.
  it('does not duplicate a row when the appended page overlaps', () => {
    const first = epSummary('a');
    const second = epSummary('b');
    const { fixture, tasksService } = setup(
      { managed: false, canEdit: true, canTrigger: true },
      { getEntryPoints: () => of({ entry_points: [first], total: 2, failed_attributes: [] }) },
    );
    const component = fixture.componentInstance;
    vi.spyOn(tasksService, 'getEntryPoints')
      .mockReturnValue(of({ entry_points: [first, second], total: 2, failed_attributes: [] }));
    component.loadMoreEntryPoints();

    expect(component.entryPoints().map(e => e.id)).toEqual(['a', 'b']);
  });

  /// The refreshed page is the server's own prefix, so anything not in it belongs
  /// behind it whatever JS thinks of the boundary pair. Comparing the two in
  /// code-unit order dropped the paged tail on every poll under a glibc or ICU
  /// collation, where `abc` sorts before `Zlib` and in JS it does not.
  it('keeps a paged tail whose attribute sorts before the page in JS order', () => {
    const inPage = epSummary('a', 'packages."x86_64-linux".abc');
    const paged = epSummary('b', 'packages."x86_64-linux".Zlib');
    const { fixture, tasksService } = setup(
      { managed: false, canEdit: true, canTrigger: true },
      { getEntryPoints: () => of({ entry_points: [inPage], total: 2, failed_attributes: [] }) },
    );
    const component = fixture.componentInstance;
    vi.spyOn(tasksService, 'getEntryPoints').mockReturnValue(of({ entry_points: [paged], total: 2, failed_attributes: [] }));
    component.loadMoreEntryPoints();
    expect(component.entryPoints().map(e => e.id)).toEqual(['a', 'b']);

    vi.spyOn(tasksService, 'getEntryPoints').mockReturnValue(of({ entry_points: [inPage], total: 2, failed_attributes: [] }));
    component.loadTaskData();

    expect(component.entryPoints().map(e => e.id)).toEqual(['a', 'b']);
  });

  /// A refresh re-reads only the window it shows, so an appended tail must not be
  /// dropped by the next live poll.
  it('keeps appended packages across a refresh of the first window', () => {
    const first = epSummary('a');
    const second = epSummary('b');
    const { fixture, tasksService } = setup(
      { managed: false, canEdit: true, canTrigger: true },
      { getEntryPoints: () => of({ entry_points: [first], total: 2, failed_attributes: [] }) },
    );
    const component = fixture.componentInstance;
    vi.spyOn(tasksService, 'getEntryPoints').mockReturnValue(of({ entry_points: [second], total: 2, failed_attributes: [] }));
    component.loadMoreEntryPoints();
    expect(component.entryPoints().map(e => e.id)).toEqual(['a', 'b']);

    vi.spyOn(tasksService, 'getEntryPoints').mockReturnValue(of({ entry_points: [first], total: 2, failed_attributes: [] }));
    component.loadTaskData();

    expect(component.entryPoints().map(e => e.id)).toEqual(['a', 'b']);
  });

  it('labels a pull-request trigger as "PR #<n>" and a manual run with the user who started it (#391)', () => {
    const { fixture } = setup({ managed: false, canEdit: true, canTrigger: true });
    const component = fixture.componentInstance;
    const pr = { ...evalSummary('pr1'), trigger: { id: 't', type: 'reporter_pull_request' as const }, pr_number: 42 };
    expect(component.triggerLabel(pr)).toBe('PR #42');
    const noNum = { ...pr, pr_number: null };
    expect(component.triggerLabel(noNum)).toBe('PR');
    expect(component.triggerLabel({ ...evalSummary('m1'), triggered_by: 'dave' })).toBe('Manual · dave');
  });

  it('preserves an explicit non-newest selection across a live refetch', () => {
    const e2 = evalSummary('e2', 'Completed');
    const { fixture } = setup(
      { managed: false, canEdit: true, canTrigger: true },
      { extraEvals: [e2] },
    );
    const component = fixture.componentInstance;

    component.select(component.evaluations()[1]);
    expect(component.selected()?.id).toBe(component.evaluations()[1].id);

    component.loadTaskData();
    expect(component.selected()?.id).toBe(component.evaluations()[1].id);
  });

  it('keeps the newest evaluation selected when a live refresh lists a newer one and offers it as a pill', () => {
    const trigger = { managed: false, canEdit: true, canTrigger: true };
    const { fixture, tasksService } = setup(trigger, { primaryStatus: 'Completed' });
    const component = fixture.componentInstance;
    const root = fixture.nativeElement as HTMLElement;
    expect(root.querySelector('.new-run-started-pill')).toBeNull();
    vi.spyOn(tasksService, 'getTask').mockReturnValue(of({
      ...taskFor(trigger, [], 'Completed'),
      last_evaluations: [evalSummary('e2', 'Queued', { commit_message: 'Bump flake inputs' }), ...component.evaluations()],
    }));
    component.loadTaskData();
    fixture.detectChanges();
    expect(component.selected()?.id).toBe('e1');
    const pill = root.querySelector<HTMLButtonElement>('.new-run-started-pill')!;
    expect(pill.textContent?.trim()).toBe('New run started: Bump flake inputs');
    pill.click();
    fixture.detectChanges();
    expect(component.selected()?.id).toBe('e2');
    expect(root.querySelector('.new-run-started-pill')).toBeNull();
  });

  it('labels a retry and a manual re-run of a commit on its tab and in the panel', () => {
    const { fixture } = setup({ managed: false, canEdit: true, canTrigger: true }, {
      primaryStatus: 'Completed',
      primary: { retry_of: 'e0' },
      extraEvals: [evalSummary('e9', 'Completed'), evalSummary('e0', 'Failed', { trigger: { id: 't', type: 'polling' as const } })],
    });
    const component = fixture.componentInstance;
    const root = fixture.nativeElement as HTMLElement;
    const labels = Array.from(root.querySelectorAll('.eval-card .evaluation-origin')).map((label) => label.textContent?.trim());
    expect(labels).toEqual(['Retry of abc1234d', 'Re-run of abc1234d']);
    const panelLink = root.querySelector<HTMLAnchorElement>('.panel-title .sub a.evaluation-origin');
    expect(panelLink?.getAttribute('href')).toBe('/project/acme/task/demo?eval=e0');
    expect(component.retryOrRerunLabelByEvaluationId().get('e0')).toBeNull();
  });

  it('names who aborted an evaluation and links a replaced evaluation to its replacement', () => {
    const { fixture } = setup({ managed: false, canEdit: true, canTrigger: true }, {
      primaryStatus: 'Aborted',
      primary: { abort_reason: 'superseded', superseded_by: 'e2', finished_at: '2026-01-01T00:05:00' },
      extraEvals: [evalSummary('e0', 'Aborted', { abort_reason: 'user', aborted_by: 'dave', finished_at: '2026-01-01T16:31:00' })],
    });
    const component = fixture.componentInstance;
    const root = fixture.nativeElement as HTMLElement;
    const replaced = root.querySelector<HTMLAnchorElement>('.panel-title a.abort-reason');
    expect(replaced?.textContent?.trim()).toMatch(/^New push: .+ replaced this evaluation$/);
    expect(replaced?.getAttribute('href')).toBe('/project/acme/task/demo?eval=e2');
    component.select(component.evaluations()[1]);
    fixture.detectChanges();
    expect(root.querySelector('.panel-title span.abort-reason')?.textContent?.trim()).toMatch(/^Aborted by dave at /);
    expect(root.querySelector('#evaluation-tab-e0 gr-status-icon')?.getAttribute('aria-label')).toMatch(/^Aborted by dave at /);
  });

  it('persists an explicit selection in the eval query param', () => {
    const e2 = evalSummary('e2', 'Completed');
    const { fixture } = setup(
      { managed: false, canEdit: true, canTrigger: true },
      { extraEvals: [e2] },
    );
    const component = fixture.componentInstance;
    const router = TestBed.inject(Router);
    const spy = vi.spyOn(router, 'navigate').mockResolvedValue(true);
    component.select(component.evaluations()[1]);
    expect(spy).toHaveBeenCalledWith([], expect.objectContaining({
      queryParams: { eval: 'e2' },
      replaceUrl: true,
    }));
  });

  it('barCounts folds the entry point own build into its dep counts', () => {
    const { fixture } = setup({ managed: false, canEdit: true, canTrigger: true });
    const component = fixture.componentInstance;
    const deps = { completed: 2, failed: 0, building: 0, queued: 0, substituted: 1, aborted: 0 };
    const ep = { deps, build_status: 'Completed' } as unknown as EntryPointSummary;
    expect(component.barCounts(ep).completed).toBe(3);
    const sub = { deps: { ...deps, completed: 0, substituted: 0 }, build_status: 'Substituted' } as unknown as EntryPointSummary;
    expect(component.barCounts(sub).substituted).toBe(1);
  });
});

describe('TaskDetailComponent - abort modal', () => {
  const edit = { managed: false, canEdit: true, canTrigger: true };
  const abortButton = (fixture: ComponentFixture<TaskDetailComponent>) =>
    (fixture.nativeElement as HTMLElement).querySelector<HTMLButtonElement>('.panel-actions button[label="Abort"]')!;

  it('asks first on a plain click of Abort, naming the task and commit, then aborts', () => {
    const { fixture, tasksService, confirm, notify } = setup(edit);
    const spy = vi.spyOn(tasksService, 'abortEvaluation').mockReturnValue(of('Success'));
    abortButton(fixture).click();
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ title: 'Abort Evaluation', target: 'Demo · abc1234d' }));
    expect(spy).toHaveBeenCalledWith('acme', 'demo', 'e1');
    expect(notify.success).toHaveBeenCalledWith('Evaluation aborted');
  });

  it('sends nothing when the abort is cancelled', () => {
    const { fixture, tasksService, confirm } = setup(edit);
    confirm.mockImplementation(() => EMPTY);
    const spy = vi.spyOn(tasksService, 'abortEvaluation');
    abortButton(fixture).click();
    expect(confirm).toHaveBeenCalledOnce();
    expect(spy).not.toHaveBeenCalled();
  });

  it('aborts at once on a Ctrl click, with no confirm modal', () => {
    const { fixture, tasksService, confirm } = setup(edit);
    const spy = vi.spyOn(tasksService, 'abortEvaluation').mockReturnValue(NEVER);
    const component = fixture.componentInstance;
    component.requestAbort(new MouseEvent('click', { ctrlKey: true }), component.evaluations()[0]);
    expect(spy).toHaveBeenCalledWith(component.projectName, component.taskName, 'e1');
    expect(confirm).not.toHaveBeenCalled();
    expect(component.aborting()).toBe(true);
  });

  it('reports a refused Ctrl-click abort as a toast, since no dialog is open', () => {
    const { fixture, tasksService, notify } = setup(edit);
    const error = new ApiError('Evaluation already finished', 409);
    vi.spyOn(tasksService, 'abortEvaluation').mockReturnValue(throwError(() => error));
    const component = fixture.componentInstance;
    component.requestAbort(new MouseEvent('click', { ctrlKey: true }), component.evaluations()[0]);
    expect(notify.failure).toHaveBeenCalledWith('Failed to abort evaluation', error);
    expect(component.aborting()).toBe(false);
  });

  it('ignores a second Ctrl click while one abort is still running', () => {
    const { fixture, tasksService } = setup(edit);
    const spy = vi.spyOn(tasksService, 'abortEvaluation').mockReturnValue(NEVER);
    const component = fixture.componentInstance;
    const ctrlClick = new MouseEvent('click', { ctrlKey: true });
    component.requestAbort(ctrlClick, component.evaluations()[0]);
    component.requestAbort(ctrlClick, component.evaluations()[0]);
    expect(spy).toHaveBeenCalledTimes(1);
  });
});

describe('TaskDetailComponent diagnostic report', () => {
  function component(): TaskDetailComponent {
    const { fixture } = setup({ managed: false, canEdit: true, canTrigger: true });
    return fixture.componentInstance;
  }

  it('offers the logs first, then metrics and the diagnostic report', () => {
    const { fixture } = setup({ managed: false, canEdit: true, canTrigger: true }, {}, 'superuser');
    const labels = fixture.componentInstance.evalMenuModel().map(i => i.label);
    expect(labels).toEqual(['Logs', 'Summary', 'Show job', 'Copy', 'Run again', 'Prioritize', 'Diagnostic report']);
  });

  it('points the logs entry at the selected evaluation', () => {
    const item = component().evalMenuModel().find(i => i.label === 'Logs');
    expect(String(item?.routerLink)).toBe('/project/acme/log/e1');
  });

  it('hands the bar the same counts for an unchanged package, so the bar is not redrawn', () => {
    const c = component();
    const ep = epSummary('a');
    expect(c.barCounts(ep)).toBe(c.barCounts(ep));
  });

  it('points the summary entry at the selected evaluation', () => {
    const item = component().evalMenuModel().find(i => i.label === 'Summary');
    expect(String(item?.routerLink)).toBe('/project/acme/summary/e1');
  });

  /// Generating a report is an authenticated endpoint, so the entry point is
  /// gone rather than present and guaranteed to fail.
  it('hides the diagnostic report from anonymous visitors', () => {
    const { fixture } = setup({ managed: false, canEdit: true, canTrigger: true }, {}, 'anonymous');
    const labels = fixture.componentInstance.evalMenuModel().map(i => i.label);
    expect(labels).toEqual(['Logs', 'Summary', 'Show job', 'Copy']);
  });

  it('opens the dialog from the menu command', () => {
    const c = component();
    const item = c.panelMenuModel().find(i => i.label === 'Diagnostic report');
    expect(item?.icon).toBe('bug_report');
    item!.command!();
    expect(c.reportDialogOpen()).toBe(true);
  });

  /// The bundled default hands over a report that names which package broke
  /// but not whose repository it is. Build logs carry whatever a build printed,
  /// so they are opted into rather than out of.
  it('defaults to hidden identities, real package names, no build logs, no instance context and no exec jobs', () => {
    const o = component().reportOptions();
    expect(o.include_identities).toBe(false);
    expect(o.include_packages).toBe(true);
    expect(o.include_logs).toBe(false);
    expect(o.include_instance).toBe(false);
    expect(o.include_exec).toBe(false);
  });

  it('toggles one option without disturbing the others', () => {
    const c = component();
    c.setReportOption('include_identities', true);
    expect(c.reportOptions().include_identities).toBe(true);
    expect(c.reportOptions().include_packages).toBe(true);
  });

  it('keeps the dialog open on Escape while the report is generated', () => {
    const { fixture } = setup({ managed: false, canEdit: true, canTrigger: true }, { downloadReport: () => NEVER });
    const c = fixture.componentInstance;
    c.openReportDialog();
    fixture.detectChanges();
    c.generateReport();
    fixture.detectChanges();
    const panel = document.querySelector('.gr-dialog') as HTMLElement;
    panel.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', keyCode: 27, bubbles: true }));
    fixture.detectChanges();
    expect(c.reportDialogOpen()).toBe(true);
    expect(panel.getAttribute('aria-busy')).toBe('true');
  });

  it('keeps the dialog open with the server reason when the report is refused', () => {
    const { fixture } = setup({ managed: false, canEdit: true, canTrigger: true }, {
      downloadReport: () => throwError(() => new ApiError('Instance context needs the Manage Workers permission', 403)),
    });
    const c = fixture.componentInstance;
    c.panelMenuModel().find(i => i.label === 'Diagnostic report')!.command!();
    c.generateReport();
    fixture.detectChanges();

    expect(c.reportDialogOpen()).toBe(true);
    expect(c.reportBusy()).toBe(false);
    const dialog = Array.from(document.body.querySelectorAll('.gr-dialog'))
      .find((el) => el.querySelector('.gr-dialog__title')?.textContent?.includes('Diagnostic report'));
    expect(dialog?.querySelector('gr-message-banner')?.textContent).toContain('Instance context needs the Manage Workers permission');
  });

  it('keeps the evaluation behind the open report dialog on an arrow key and exports the one it opened for', () => {
    const downloadReport = vi.fn(() => NEVER);
    const { fixture, tasksService } = setup({ managed: false, canEdit: true, canTrigger: true }, { extraEvals: [evalSummary('e2', 'Completed')] });
    vi.spyOn(tasksService, 'downloadReport').mockImplementation(downloadReport);
    const c = fixture.componentInstance;
    c.openReportDialog();
    fixture.detectChanges();
    const panel = document.querySelector('.gr-dialog') as HTMLElement;
    panel.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true, cancelable: true }));
    expect(c.selected()?.id).toBe('e1');

    c.select(c.evaluations()[1]);
    c.generateReport();
    expect(downloadReport).toHaveBeenCalledWith('e1', expect.anything());
  });

  it('leaves Ctrl+F to the browser while focus is outside the evaluation panel', () => {
    const { fixture } = setup({ managed: false, canEdit: true, canTrigger: true });
    const find = new KeyboardEvent('keydown', { key: 'f', ctrlKey: true, cancelable: true, bubbles: true });
    document.body.dispatchEvent(find);
    expect(find.defaultPrevented).toBe(false);
    expect(fixture.nativeElement.querySelector(".pkg-search input")).toBeNull();
  });

  it('clears the previous report error when the dialog opens again', () => {
    const { fixture } = setup({ managed: false, canEdit: true, canTrigger: true }, {
      downloadReport: () => throwError(() => new Error('Http failure response for /report: 403 Forbidden')),
    });
    const c = fixture.componentInstance;
    c.openReportDialog();
    c.generateReport();
    expect(c.reportError()).toBe('Failed to generate the diagnostic report.');

    c.reportDialogOpen.set(false);
    c.openReportDialog();
    expect(c.reportError()).toBeNull();
  });
});

describe('TaskDetailComponent package sbom', () => {
  const access = { managed: false, canEdit: true, canTrigger: true };
  const objectUrls = URL as unknown as Record<string, unknown>;

  function sbomItem(fixture: ComponentFixture<TaskDetailComponent>, ep: EntryPointSummary) {
    fixture.componentInstance.openPkgMenu(new Event('click'), ep, 'e1', { toggle: () => {} });
    return fixture.componentInstance.pkgMenuModel().find(i => i.label === 'SBOM');
  }

  afterEach(() => {
    delete objectUrls['createObjectURL'];
    delete objectUrls['revokeObjectURL'];
  });

  it('no longer offers an sbom for the whole evaluation', () => {
    const { fixture } = setup(access);
    expect(menuLabels(fixture)).not.toContain('SBOM');
  });

  it('says why the sbom of an entry point without outputs waits', () => {
    const { fixture } = setup(access);
    expect(sbomItem(fixture, { ...epSummary('hello'), build_status: 'Building' })?.disabledReason).toBe('The SBOM is available after a successful build.');
  });

  it('downloads the sbom of a built entry point under the server filename', () => {
    const { fixture, evaluationsService } = setup(access, {}, 'anonymous');
    const download = vi.fn(() => of(new HttpResponse({
      body: new Blob(['{}']),
      headers: new HttpHeaders({ 'content-disposition': 'attachment; filename="hello-2.12.3-sbom.cdx.json"' }),
    })));
    (evaluationsService as unknown as Record<string, unknown>)['downloadBuildSbom'] = download;
    objectUrls['createObjectURL'] = () => 'blob:sbom';
    objectUrls['revokeObjectURL'] = () => undefined;
    const clicked: HTMLAnchorElement[] = [];
    vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(function (this: HTMLAnchorElement) {
      clicked.push(this);
    });

    const item = sbomItem(fixture, epSummary('hello'));
    expect(item?.disabledReason).toBeNull();
    item!.command!();

    expect(download).toHaveBeenCalledWith('b-hello');
    expect(clicked[0].download).toBe('hello-2.12.3-sbom.cdx.json');
  });

  it('shows the error when the download fails', () => {
    const { fixture, evaluationsService, notify } = setup(access);
    const error = new ApiError('Build not found', 404);
    (evaluationsService as unknown as Record<string, unknown>)['downloadBuildSbom'] = () => throwError(() => error);

    sbomItem(fixture, epSummary('hello'))!.command!();

    expect(notify.failure).toHaveBeenCalledWith('Failed to download SBOM', error);
  });
});

describe('TaskDetailComponent - vertical fill', () => {
  it('grows into the task shell and lets the panel absorb the leftover height', () => {
    const { fixture } = setup({ managed: false, canEdit: true, canTrigger: true });
    const host = getComputedStyle(fixture.nativeElement as HTMLElement);
    expect(host.display).toBe('flex');
    expect(host.flexDirection).toBe('column');
    expect(host.flexGrow).toBe('1');

    const panel = (fixture.nativeElement as HTMLElement).querySelector('.panel');
    expect(panel).not.toBeNull();
    expect(getComputedStyle(panel!).flexGrow).toBe('1');
  });
});

describe('TaskDetailComponent - #636 eval page', () => {
  const access = { managed: false, canEdit: true, canTrigger: true };

  it('steps the strip on a bare arrow but leaves modified arrows to the browser', () => {
    const { fixture } = setup(access, { extraEvals: [evalSummary('e2', 'Completed')] });
    const comp = fixture.componentInstance;
    const raf = vi.spyOn(window, 'requestAnimationFrame').mockReturnValue(0);
    const pressed = (init: KeyboardEventInit) => {
      const e = new KeyboardEvent('keydown', { key: 'ArrowRight', cancelable: true, ...init });
      comp.onEvaluationTabListKeydown(e);
      return e.defaultPrevented;
    };
    expect(pressed({ altKey: true })).toBe(false);
    expect(pressed({ ctrlKey: true })).toBe(false);
    expect(comp.selected()?.id).toBe('e1');
    expect(pressed({})).toBe(true);
    expect(comp.selected()?.id).toBe('e2');
    raf.mockRestore();
  });

  it('dates each evaluation tab and scrolls the strip from its arrow buttons', () => {
    const { fixture } = setup(access, { extraEvals: [evalSummary('e2', 'Completed')] });
    const root = fixture.nativeElement as HTMLElement;
    const times = Array.from(root.querySelectorAll('.eval-card time'));
    expect(times.map((time) => time.textContent)).toEqual([monthDayClockTime('2026-01-01T00:00:00'), monthDayClockTime('2026-01-01T00:00:00')]);

    const strip = root.querySelector<HTMLElement>('.strip-wrap')!;
    const scrollBy = vi.fn();
    Object.assign(strip, { scrollBy });
    Object.defineProperties(strip, { clientWidth: { value: 500 }, scrollWidth: { value: 1200 } });
    window.dispatchEvent(new Event('resize'));
    fixture.detectChanges();
    expect(root.querySelector('.strip-arrow--start')?.classList).toContain('strip-arrow--at-edge');
    expect(root.querySelector('.strip-arrow--end')?.classList).not.toContain('strip-arrow--at-edge');
    root.querySelector<HTMLButtonElement>('.strip-arrow--end')!.click();
    expect(scrollBy).toHaveBeenCalledWith({ left: 400, behavior: 'smooth' });
  });

  it('offers the evaluations as keyboard tabs and moves focus with the arrows', () => {
    const { fixture } = setup(access, { extraEvals: [evalSummary('e2', 'Completed')] });
    const frames: FrameRequestCallback[] = [];
    const raf = vi.spyOn(window, 'requestAnimationFrame').mockImplementation((callback) => frames.push(callback));
    HTMLElement.prototype.scrollIntoView ??= () => {};
    const root = fixture.nativeElement as HTMLElement;
    const tabs = () => Array.from(root.querySelectorAll<HTMLButtonElement>('[role="tablist"] button[role="tab"]'));
    expect(tabs().map((tab) => [tab.getAttribute('aria-selected'), tab.tabIndex])).toEqual([['true', 0], ['false', -1]]);
    expect(root.querySelector('[role="tabpanel"]')?.getAttribute('aria-labelledby')).toBe(tabs()[0].id);

    tabs()[0].focus();
    tabs()[0].dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true, cancelable: true }));
    fixture.detectChanges();
    frames.forEach((frame) => frame(0));
    expect(fixture.componentInstance.selected()?.id).toBe('e2');
    expect(document.activeElement).toBe(tabs()[1]);
    expect(tabs()[1].tabIndex).toBe(0);
    raf.mockRestore();
  });

  it('jumps to the last and first evaluation tab with End and Home', () => {
    const { fixture } = setup(access, { extraEvals: [evalSummary('e2', 'Completed'), evalSummary('e3', 'Completed')] });
    const raf = vi.spyOn(window, 'requestAnimationFrame').mockImplementation(() => 0);
    HTMLElement.prototype.scrollIntoView ??= () => {};
    const root = fixture.nativeElement as HTMLElement;
    const press = (key: string) => {
      const tab = root.querySelector<HTMLButtonElement>('[role="tab"].selected')!;
      tab.focus();
      tab.dispatchEvent(new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true }));
      fixture.detectChanges();
    };
    press('End');
    expect(fixture.componentInstance.selected()?.id).toBe('e3');
    press('Home');
    expect(fixture.componentInstance.selected()?.id).toBe('e1');
    raf.mockRestore();
  });

  it('shows a waiting state instead of "No packages" while the evaluation is in progress', () => {
    const { fixture } = setup(access, { primaryStatus: 'EvaluatingDerivation' });
    expect(fixture.nativeElement.textContent).not.toContain('No packages');
    expect(fixture.nativeElement.querySelector('.panel gr-loading-spinner')).toBeNull();
    const wait = fixture.nativeElement.querySelector('.panel .pkg-wait');
    expect(wait?.querySelector('gr-icon')).toBeTruthy();
    expect(wait?.textContent).toContain('Waiting for packages');
  });

  it('says "No packages" once a finished evaluation has none', () => {
    const { fixture } = setup(access, { primaryStatus: 'Completed' });
    expect(fixture.nativeElement.textContent).toContain('No packages');
  });

  it('offers the closure of a built entry point', () => {
    const { fixture } = setup(access, {
      getEntryPoints: () => of({ entry_points: [epSummary('hello')], total: 1, failed_attributes: [] }),
    });
    const comp = fixture.componentInstance;
    comp.openPkgContextMenu(new MouseEvent('contextmenu'), epSummary('hello'), 'e1', { openAt: () => {} });
    const closure = comp.pkgMenuModel().find(i => i.label === 'View closure');
    expect(closure?.disabledReason).toBeNull();
    expect(String(closure?.routerLink)).toBe('/project/acme/closure/build/b-hello?task=demo&eval=e1');
  });

  it('links each package with a real href, so a middle click opens a new tab', () => {
    const { fixture } = setup(access, {
      getEntryPoints: () => of({ entry_points: [epSummary('hello')], total: 1, failed_attributes: [] }),
    });
    fixture.detectChanges();
    const link = fixture.nativeElement.querySelector('.pkg a.entry-link') as HTMLAnchorElement;
    expect(link.getAttribute('href')).toBe('/project/acme/log/e1?package=b-hello');
  });

  it('loads the next page when the end of the list scrolls into view', () => {
    const { fixture, tasksService } = setup(access, {
      getEntryPoints: () => of({ entry_points: [epSummary('a')], total: 2, failed_attributes: [] }),
    });
    fixture.detectChanges();
    const spy = vi.spyOn(tasksService, 'getEntryPoints').mockReturnValue(of({ entry_points: [epSummary('b')], total: 2, failed_attributes: [] }));
    const sentinel = fixture.debugElement.query(By.css('.pkg-more'));
    expect(sentinel.nativeElement.querySelectorAll('gr-skeleton').length).toBeGreaterThan(0);
    expect(sentinel.nativeElement.textContent.trim()).toBe('');
    sentinel.triggerEventHandler('grInView');
    expect(spy).toHaveBeenCalledWith('acme', 'demo', 'e1', 25, 1, '');
    fixture.detectChanges();
    expect(fixture.nativeElement.querySelector('.pkg-more')).toBeNull();
  });

  it('opens the package menu on right-click', () => {
    const { fixture } = setup(access, {
      getEntryPoints: () => of({ entry_points: [epSummary('hello')], total: 1, failed_attributes: [] }),
    });
    fixture.detectChanges();
    const comp = fixture.componentInstance;
    const spy = vi.spyOn(comp, 'openPkgContextMenu');
    const row = fixture.nativeElement.querySelector('.pkg') as HTMLElement;
    row.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true }));
    expect(spy).toHaveBeenCalled();
  });

  it('opens the evaluation menu for the right-clicked evaluation', () => {
    const { fixture } = setup(access, { extraEvals: [evalSummary('e2', 'Completed')] });
    fixture.detectChanges();
    const cards = fixture.nativeElement.querySelectorAll('.eval-card') as NodeListOf<HTMLElement>;
    const event = new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: 10, clientY: 10 });
    cards[1].dispatchEvent(event);
    fixture.detectChanges();
    expect(event.defaultPrevented).toBe(true);
    expect(fixture.componentInstance.selected()?.id).toBe('e2');
    const summary = [...document.querySelectorAll('.cdk-overlay-container a')].find(a => a.textContent?.includes('Summary'));
    expect(summary?.getAttribute('href')).toContain('/summary/e2');
  });
});

describe('TaskDetailComponent - status phases', () => {
  const access = { managed: false, canEdit: true, canTrigger: true };

  it('marks the evaluation card and its icon with the evaluation phase', () => {
    const { fixture } = setup(access, { primaryStatus: 'Building' });
    const card: HTMLElement = fixture.nativeElement.querySelector('.eval-card');
    expect(card.dataset['phase']).toBe('running');
    expect(card.querySelector('gr-status-icon.phase-swatch')?.getAttribute('data-phase')).toBe('running');
  });

  it('marks a package row and its icon with the build phase', () => {
    const skipped = { ...epSummary('a'), build_status: 'Skipped' as const };
    const { fixture } = setup(access, { getEntryPoints: () => of({ entry_points: [skipped], total: 1, failed_attributes: [] }) });
    fixture.detectChanges();
    const pkg: HTMLElement = fixture.nativeElement.querySelector('.pkg');
    expect(pkg.dataset['phase']).toBe('aborted');
    expect(pkg.querySelector('gr-status-icon.phase-swatch')?.getAttribute('data-phase')).toBe('aborted');
  });
});

describe('TaskDetailComponent prioritize', () => {
  const trigger = { managed: false, canEdit: true, canTrigger: true };
  const pkgLabels = (fixture: ComponentFixture<TaskDetailComponent>, ep: EntryPointSummary) => {
    fixture.componentInstance.openPkgMenu(new Event('click'), ep, 'e1', { toggle: () => {} });
    return fixture.componentInstance.pkgMenuModel().map(i => i.label);
  };

  it('prioritizes a running evaluation from the panel menu, then refreshes', () => {
    const { fixture, tasksService, evaluationsService } = setup(trigger, { primaryStatus: 'Building' }, 'superuser');
    const spy = vi.spyOn(evaluationsService, 'prioritizeEvaluation');
    const reload = vi.spyOn(tasksService, 'getTask');
    fixture.componentInstance.panelMenuModel().find(i => i.label === 'Prioritize')!.command!();
    expect(spy).toHaveBeenCalledWith('e1');
    expect(reload).toHaveBeenCalled();
  });

  it('offers no evaluation prioritize once the evaluation finished', () => {
    const { fixture } = setup(trigger, { primaryStatus: 'Completed' }, 'superuser');
    expect(menuLabels(fixture)).not.toContain('Prioritize');
  });

  it('offers no evaluation prioritize once it is prioritized', () => {
    const { fixture } = setup(trigger, { primaryStatus: 'Building', primary: { prioritized: true } }, 'superuser');
    expect(menuLabels(fixture)).not.toContain('Prioritize');
  });

  it('offers no evaluation prioritize to a member with edit access who is not a superuser', () => {
    const { fixture } = setup(trigger, { primaryStatus: 'Building' });
    expect(menuLabels(fixture)).not.toContain('Prioritize');
  });

  it('offers no evaluation prioritize to an anonymous visitor', () => {
    const { fixture } = setup(trigger, { primaryStatus: 'Building' }, 'anonymous');
    expect(menuLabels(fixture)).not.toContain('Prioritize');
  });

  it('reports a failed prioritize', () => {
    const { fixture, evaluationsService, notify } = setup(trigger, { primaryStatus: 'Building' }, 'superuser');
    const error = new ApiError('Evaluation already finished', 409);
    vi.spyOn(evaluationsService, 'prioritizeEvaluation').mockReturnValue(throwError(() => error));
    fixture.componentInstance.panelMenuModel().find(i => i.label === 'Prioritize')!.command!();
    expect(notify.failure).toHaveBeenCalledWith('Failed to prioritize evaluation', error);
  });

  it('prioritizes a pending entry point by its build id', () => {
    const { fixture, evaluationsService } = setup(trigger, {}, 'superuser');
    const spy = vi.spyOn(evaluationsService, 'prioritizeBuild');
    const ep = { ...epSummary('hello'), build_status: 'Queued' } as EntryPointSummary;
    expect(pkgLabels(fixture, ep)).toContain('Prioritize');
    fixture.componentInstance.pkgMenuModel().find(i => i.label === 'Prioritize')!.command!();
    expect(spy).toHaveBeenCalledWith('b-hello');
  });

  it('offers no entry point prioritize once its build finished', () => {
    const { fixture } = setup(trigger, {}, 'superuser');
    expect(pkgLabels(fixture, epSummary('hello'))).not.toContain('Prioritize');
  });

  it('offers no entry point prioritize once it is prioritized', () => {
    const { fixture } = setup(trigger, {}, 'superuser');
    const ep = { ...epSummary('hello'), build_status: 'Queued', prioritized: true } as EntryPointSummary;
    expect(pkgLabels(fixture, ep)).not.toContain('Prioritize');
  });

  it('offers no entry point prioritize to a member with edit access who is not a superuser', () => {
    const { fixture } = setup(trigger);
    const ep = { ...epSummary('hello'), build_status: 'Building' } as EntryPointSummary;
    expect(pkgLabels(fixture, ep)).not.toContain('Prioritize');
  });
});

describe('TaskDetailComponent entry point abort and retry', () => {
  const edit = { managed: false, canEdit: true, canTrigger: true };
  const readOnly = { managed: false, canEdit: false, canTrigger: false };
  const entryPoint = (build_status: EntryPointSummary['build_status']) => ({ ...epSummary('hello'), build_status });
  const pkgLabels = (fixture: ComponentFixture<TaskDetailComponent>, ep: EntryPointSummary, evaluation = 'e1') => {
    fixture.componentInstance.openPkgMenu(new Event('click'), ep, evaluation, { toggle: () => {} });
    return fixture.componentInstance.pkgMenuModel().map(i => i.label);
  };
  const run = (fixture: ComponentFixture<TaskDetailComponent>, label: string) =>
    fixture.componentInstance.pkgMenuModel().find(i => i.label === label)!.command!();

  it('asks first, naming the entry point, then aborts it by its build id', () => {
    const { fixture, evaluationsService, confirm } = setup(edit);
    const spy = vi.spyOn(evaluationsService, 'abortBuild');
    const labels = pkgLabels(fixture, entryPoint('Building'));
    expect(labels).toContain('Abort');
    expect(labels).not.toContain('Retry');
    run(fixture, 'Abort');
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ title: 'Abort Build', target: 'packages."x86_64-linux".hello · Demo' }));
    expect(spy).toHaveBeenCalledWith('b-hello');
  });

  it('sends no entry point abort when it is cancelled', () => {
    const { fixture, evaluationsService, confirm } = setup(edit);
    confirm.mockImplementation(() => EMPTY);
    const spy = vi.spyOn(evaluationsService, 'abortBuild');
    pkgLabels(fixture, entryPoint('Building'));
    run(fixture, 'Abort');
    expect(spy).not.toHaveBeenCalled();
  });

  it('retries a failed entry point by its build id', () => {
    const { fixture, evaluationsService } = setup(edit);
    const spy = vi.spyOn(evaluationsService, 'retryBuild');
    const labels = pkgLabels(fixture, entryPoint('DependencyFailed'));
    expect(labels).toContain('Retry');
    expect(labels).not.toContain('Abort');
    run(fixture, 'Retry');
    expect(spy).toHaveBeenCalledWith('b-hello');
  });

  it('offers no single build retry once the evaluation finished, leaving Retry failed builds as the way', () => {
    const { fixture } = setup(edit, { primaryStatus: 'Failed', primary: failedBuilds });
    expect(pkgLabels(fixture, entryPoint('FailedPermanent'))).not.toContain('Retry');
    expect(menuLabels(fixture)).toContain('Retry failed builds');
  });

  it('offers neither abort nor retry without edit access', () => {
    const { fixture } = setup(readOnly);
    expect(pkgLabels(fixture, entryPoint('Building'))).not.toContain('Abort');
    expect(pkgLabels(fixture, entryPoint('Aborted'))).not.toContain('Retry');
  });
});

describe('TaskDetailComponent header star', () => {
  const access = { managed: false, canEdit: false, canTrigger: false };

  it('stars the task from its header', () => {
    const { fixture } = setup(access);
    const stars = TestBed.inject(StarsService);
    expect(stars.starred).toHaveBeenCalledWith({ kind: 'task', project: 'acme', task: 'demo' });
    const button = (fixture.nativeElement as HTMLElement).querySelector('gr-star-button button');
    expect(button!.getAttribute('aria-pressed')).toBe('true');
  });

  it('shows no star to a guest', () => {
    const { fixture } = setup(access, {}, 'anonymous');
    expect((fixture.nativeElement as HTMLElement).querySelector('gr-star-button')).toBeNull();
  });
});

describe('TaskDetailComponent repository chip', () => {
  const access = { managed: false, canEdit: false, canTrigger: false };
  const repositoryChip = (fixture: ComponentFixture<TaskDetailComponent>) =>
    (fixture.nativeElement as HTMLElement).querySelector('.meta-chips > :first-child') as HTMLElement;

  it('makes the whole chip the link for a web repository', () => {
    const { fixture } = setup(access, { repository: 'https://github.com/acme/demo' });
    const chip = repositoryChip(fixture);
    expect(chip.tagName).toBe('A');
    expect(chip.getAttribute('href')).toBe('https://github.com/acme/demo');
    expect(chip.querySelector('gr-icon')).not.toBeNull();
  });

  it('links an ssh repository to its https page', () => {
    const { fixture } = setup(access, { repository: 'ssh://git@github.com/acme/demo.git' });
    expect(repositoryChip(fixture).getAttribute('href')).toBe('https://github.com/acme/demo');
  });

  it('keeps a local repository a plain chip', () => {
    const { fixture } = setup(access, { repository: '/srv/git/demo' });
    const chip = repositoryChip(fixture);
    expect(chip.tagName).toBe('SPAN');
    expect(chip.querySelector('a')).toBeNull();
  });

  it('shows the flake directory as its own chip and links the repository without it', () => {
    const { fixture } = setup(access, { repository: 'https://github.com/acme/infra?dir=nix/hosts' });
    const chip = repositoryChip(fixture);
    expect(chip.getAttribute('href')).toBe('https://github.com/acme/infra');
    expect(chip.textContent).not.toContain('?dir');
    expect((chip.nextElementSibling as HTMLElement).textContent).toContain('dir nix/hosts');
  });

  it('shows no flake directory chip for the repository root', () => {
    const { fixture } = setup(access, { repository: 'https://github.com/acme/infra' });
    expect((fixture.nativeElement as HTMLElement).querySelector('[title="Flake directory"]')).toBeNull();
  });

  it('links the selected commit hash to the commit page', () => {
    const { fixture } = setup(access, { repository: 'git@github.com:acme/demo.git' });
    const link = (fixture.nativeElement as HTMLElement).querySelector('.panel-title a.commit-link');
    expect(link!.getAttribute('href')).toBe('https://github.com/acme/demo/commit/abc1234def5678');
  });

  it('names the selected commit once in the panel header', () => {
    const { fixture } = setup(access, { repository: 'git@github.com:acme/demo.git' });
    const head = (fixture.nativeElement as HTMLElement).querySelector('.panel-head') as HTMLElement;
    expect(head.textContent!.match(/abc1234d/g)).toHaveLength(1);
    expect(head.querySelector('.panel-title > gr-status-icon')).toBeNull();
  });
});

describe('TaskDetailComponent - eval query param', () => {
  it('selects the evaluation a changed ?eval= names and loads its entry points', () => {
    const { fixture, tasksService, query } = setup(
      { managed: false, canEdit: true, canTrigger: true },
      { extraEvals: [evalSummary('e2', 'Completed')] },
    );
    const spy = vi.spyOn(tasksService, 'getEntryPoints');
    expect(fixture.componentInstance.selected()?.id).toBe('e1');

    query.next(convertToParamMap({ eval: 'e2' }));
    fixture.detectChanges();

    expect(fixture.componentInstance.selected()?.id).toBe('e2');
    expect(spy).toHaveBeenCalledWith('acme', 'demo', 'e2', 25, 0, '');
  });

  it('shows the packages of the evaluation in the header when ?eval= names one not in this task', () => {
    const getEntryPoints = vi.fn(() => of({ entry_points: [epSummary('pkg')], total: 1, failed_attributes: [] }));
    const { fixture } = setup(
      { managed: false, canEdit: true, canTrigger: true },
      { primaryStatus: 'Completed', getEntryPoints, getEvaluation: () => throwError(() => new Error('404')) } as SetupOverrides,
      'member',
      { eval: 'gone' },
    );
    fixture.detectChanges();
    expect(fixture.componentInstance.selected()?.id).toBe('e1');
    expect(fixture.componentInstance.entryPoints().map((e) => e.id)).toEqual(['pkg']);
    expect(fixture.nativeElement.querySelector('gr-message-banner')?.textContent).toContain('Evaluation not found');
  });

  it('keeps quiet about an older ?eval= evaluation of this task', () => {
    const { fixture } = setup(
      { managed: false, canEdit: true, canTrigger: true },
      { getEvaluation: () => of({ project_name: 'acme', task_name: 'demo', error: null }) },
      'member',
      { eval: 'older' },
    );
    fixture.detectChanges();
    expect(fixture.componentInstance.requestedEvaluationNotFound()).toBe(false);
  });
});

describe('TaskDetailComponent - package search', () => {
  const access = { managed: false, canEdit: true, canTrigger: true };
  const failed = (name: string) => ({ eval: `packages."x86_64-linux".${name}`, message: 'boom' });
  const page = (names: string[], failures: string[] = []) =>
    of({ entry_points: names.map((n) => epSummary(n)), total: names.length, failed_attributes: failures.map(failed) });
  const names = (root: HTMLElement) => Array.from(root.querySelectorAll('.pkg .pkg-name'), (n) => n.textContent?.trim());

  const searchField = (fixture: ComponentFixture<TaskDetailComponent>) =>
    fixture.nativeElement.querySelector('.pkg-search input') as HTMLInputElement | null;

  function openSearch(fixture: ComponentFixture<TaskDetailComponent>): { find: KeyboardEvent; input: HTMLInputElement } {
    const find = new KeyboardEvent('keydown', { key: 'f', ctrlKey: true, cancelable: true, bubbles: true });
    fixture.nativeElement.querySelector('#evaluation-detail-panel').dispatchEvent(find);
    fixture.detectChanges();
    TestBed.tick();
    return { find, input: searchField(fixture)! };
  }

  function type(fixture: ComponentFixture<TaskDetailComponent>, input: HTMLInputElement, term: string): void {
    input.value = term;
    input.dispatchEvent(new Event('input'));
    vi.advanceTimersByTime(200);
    fixture.detectChanges();
  }

  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it('keeps the search hidden for a list that fits a page until Ctrl+F opens it focused', () => {
    const { fixture } = setup(access, { getEntryPoints: () => page(['hello']) });
    expect(searchField(fixture)).toBeNull();

    const { find, input } = openSearch(fixture);

    expect(find.defaultPrevented).toBe(true);
    expect(document.activeElement).toBe(input);
  });

  it('shows the search without the shortcut for a list spanning more than a page', () => {
    const { fixture } = setup(access, {
      getEntryPoints: () => of({ entry_points: [epSummary('hello')], total: 26, failed_attributes: [] }),
    });

    expect(searchField(fixture)).not.toBeNull();
  });

  it('keeps the same search field while the panel switches to another evaluation', () => {
    const { fixture, query } = setup(access, {
      extraEvals: [evalSummary('e2', 'Completed')],
      getEntryPoints: () => of({ entry_points: [epSummary('hello')], total: 26, failed_attributes: [] }),
    });
    const shown = searchField(fixture);
    const title = () => fixture.nativeElement.querySelector('.panel-title');
    const firstTitle = title();

    query.next(convertToParamMap({ eval: 'e2' }));
    fixture.detectChanges();

    expect(title()).not.toBe(firstTitle);
    expect(shown).not.toBeNull();
    expect(searchField(fixture)).toBe(shown);
  });

  it('asks the server for the typed term and lists the matches only', () => {
    const { fixture, tasksService } = setup(access, { getEntryPoints: () => page(['hello', 'world']) });
    const spy = vi.spyOn(tasksService, 'getEntryPoints').mockReturnValue(page(['hello']));

    type(fixture, openSearch(fixture).input, 'hel');

    expect(spy).toHaveBeenLastCalledWith('acme', fixture.componentInstance.taskName, 'e1', 25, 0, 'hel');
    expect(names(fixture.nativeElement)).toEqual(['hello']);
  });

  it('narrows the attributes that failed to evaluate by the same term', () => {
    const { fixture } = setup(access, { getEntryPoints: () => page([], ['broken', 'other']) });

    type(fixture, openSearch(fixture).input, 'BROK');

    expect(names(fixture.nativeElement)).toEqual(['broken']);
  });

  it('says that nothing matches instead of "No packages"', () => {
    const { fixture, tasksService } = setup(access, { getEntryPoints: () => page(['hello']) });
    vi.spyOn(tasksService, 'getEntryPoints').mockReturnValue(page([]));

    type(fixture, openSearch(fixture).input, 'zzz');

    expect(fixture.nativeElement.textContent).toContain('No matching packages');
  });

  it('closes on Escape and brings the full list back', () => {
    const { fixture, tasksService } = setup(access, { getEntryPoints: () => page(['hello', 'world']) });
    const spy = vi.spyOn(tasksService, 'getEntryPoints').mockReturnValue(page(['hello']));
    const { input } = openSearch(fixture);
    type(fixture, input, 'hel');
    spy.mockReturnValue(page(['hello', 'world']));

    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }));
    fixture.detectChanges();

    expect(searchField(fixture)).toBeNull();
    expect(spy).toHaveBeenLastCalledWith('acme', fixture.componentInstance.taskName, 'e1', 25, 0, '');
    expect(names(fixture.nativeElement)).toEqual(['hello', 'world']);
  });

  it('clears and leaves the search field on Escape while the list spans pages and keeps the field shown', () => {
    const { fixture } = setup(access, {
      getEntryPoints: () => of({ entry_points: [epSummary('hello')], total: 26, failed_attributes: [] }),
    });
    const input = searchField(fixture)!;
    input.focus();
    type(fixture, input, 'hel');

    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }));
    fixture.detectChanges();

    expect(searchField(fixture)).toBe(input);
    expect(input.value).toBe('');
    expect(document.activeElement).not.toBe(input);
  });
});

describe('TaskDetailComponent - live package reloads', () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it('reloads the packages once after the throttle window so the last live change shows', () => {
    const { tasksService, frames } = setup({ managed: false, canEdit: true, canTrigger: true });
    const entryPoints = vi.spyOn(tasksService, 'getEntryPoints');
    frames.next({ event: 'evaluation.status', at: '', content: {} } as LiveEvent);
    vi.advanceTimersByTime(500);
    frames.next({ event: 'evaluation.status', at: '', content: {} } as LiveEvent);
    vi.advanceTimersByTime(500);
    expect(entryPoints).not.toHaveBeenCalled();
    vi.advanceTimersByTime(3000);
    expect(entryPoints).toHaveBeenCalledTimes(1);
  });

  it('reloads the packages at once when the selected evaluation changes status', () => {
    const access = { managed: false, canEdit: true, canTrigger: true };
    const getTask = vi.fn(() => of(resolvedTask(access, {})));
    const { tasksService, frames } = setup(access, { getTask });
    const entryPoints = vi.spyOn(tasksService, 'getEntryPoints');
    getTask.mockReturnValue(of(resolvedTask(access, { primaryStatus: 'Completed' })));
    frames.next({ event: 'evaluation.status', at: '', content: {} } as LiveEvent);
    vi.advanceTimersByTime(500);
    expect(entryPoints).toHaveBeenCalledTimes(1);
  });

  it('keeps a started evaluation selected while a refresh still lists only the older ones', () => {
    const access = { managed: false, canEdit: true, canTrigger: true };
    const getTask = vi.fn(() => of(resolvedTask(access, {})));
    const { fixture } = setup(access, { getTask, startEvaluation: () => of({ id: 'e-new', name: 'acme/demo', url: '' }) });
    const cmp = fixture.componentInstance;
    cmp.startEvaluation();
    expect(cmp.selected()?.id).toBe('e1');
    getTask.mockReturnValue(of(resolvedTask(access, { extraEvals: [evalSummary('e-new', 'Queued')] })));
    cmp.loadTaskData(true);
    expect(cmp.selected()?.id).toBe('e-new');
  });

  it('keeps a fast failure when the start request answers after the live reload', () => {
    const access = { managed: false, canEdit: true, canTrigger: true };
    const responses: Subject<ReturnType<typeof taskFor>>[] = [];
    const getTask = vi.fn(() => {
      const response = new Subject<ReturnType<typeof taskFor>>();
      responses.push(response);
      return response;
    });
    const { fixture, frames } = setup(access, { getTask, primaryStatus: 'Completed', startEvaluation: () => of({ id: 'e2', name: 'acme/demo', url: '' }) });
    vi.spyOn(TestBed.inject(Router), 'navigate').mockResolvedValue(true);
    const cmp = fixture.componentInstance;
    const withLatest = (status: EvaluationSummary['status'], builds: Partial<BuildStatusCounts>) =>
      ({ ...taskFor(access), last_evaluations: [evalSummary('e2', status, { builds: { ...zeroCounts(), ...builds } }), evalSummary('e1', 'Completed')] });

    cmp.startEvaluation();
    frames.next({ event: 'evaluation.building', at: '', content: { evaluation_id: 'e2' } } as LiveEvent);
    vi.advanceTimersByTime(300);
    frames.next({ event: 'evaluation.failed', at: '', content: { evaluation_id: 'e2' } } as LiveEvent);
    vi.advanceTimersByTime(200);
    expect(responses).toHaveLength(2);

    responses[1].next(withLatest('Failed', { failed: 1 }));
    responses[1].complete();
    responses[0].next(withLatest('Building', { building: 1 }));
    responses[0].complete();
    fixture.detectChanges();

    expect(cmp.selected()?.status).toBe('Failed');
    expect(cmp.evaluationInProgress()).toBe(false);
    expect(cmp.starting()).toBe(false);
    expect((fixture.nativeElement as HTMLElement).querySelector('.panel-head gr-eval-status-badge')?.textContent?.toLowerCase()).toContain('failed');
  });

  it('drops an older live reload that answers after a newer one', () => {
    const access = { managed: false, canEdit: true, canTrigger: true };
    const responses: Subject<ReturnType<typeof taskFor>>[] = [];
    const getTask = vi.fn(() => {
      const response = new Subject<ReturnType<typeof taskFor>>();
      responses.push(response);
      return response;
    });
    const { fixture, frames } = setup(access, { getTask });
    const cmp = fixture.componentInstance;

    frames.next({ event: 'evaluation.building', at: '', content: {} } as LiveEvent);
    vi.advanceTimersByTime(500);
    frames.next({ event: 'evaluation.failed', at: '', content: {} } as LiveEvent);
    vi.advanceTimersByTime(500);
    expect(responses).toHaveLength(2);

    responses[1].next(resolvedTask(access, { primaryStatus: 'Failed' }));
    responses[0].next(resolvedTask(access, { primaryStatus: 'Building' }));
    expect(cmp.selected()?.status).toBe('Failed');

    frames.next({ event: 'evaluation.queued', at: '', content: {} } as LiveEvent);
    vi.advanceTimersByTime(500);
    responses[2].next(resolvedTask(access, { primaryStatus: 'Queued' }));
    expect(cmp.selected()?.status).toBe('Queued');
  });

  it('shows a changed waiting reason without waiting for another update', () => {
    const access = { managed: false, canEdit: true, canTrigger: true };
    const waiting = (waiting_reason: EvaluationSummary['waiting_reason']) => resolvedTask(access, { primaryStatus: 'Waiting', primary: { waiting_reason } });
    const getTask = vi.fn(() => of(waiting({ kind: 'workers', unmet: [], connected_workers: 0, available_architectures: [] })));
    const { fixture } = setup(access, { getTask });
    getTask.mockReturnValue(of(waiting({ kind: 'eval_workers', capability: 'eval', connected_workers: 0 })));
    fixture.componentInstance.loadTaskData(true);
    fixture.detectChanges();
    expect((fixture.nativeElement as HTMLElement).textContent).toContain('Waiting for evaluation worker');
  });
});
