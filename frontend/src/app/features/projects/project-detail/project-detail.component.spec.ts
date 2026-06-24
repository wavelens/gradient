/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute, Router, convertToParamMap, provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { vi } from 'vitest';
import { of, throwError } from 'rxjs';
import { ProjectDetailComponent } from './project-detail.component';
import { ProjectsService } from '@core/services/projects.service';
import { OrganizationsService } from '@core/services/organizations.service';
import { AuthService } from '@core/services/auth.service';
import { AccessState } from '@core/models/access.model';
import { BuildStatusCounts, EntryPointSummary, EvaluationSummary } from '@core/models/project.model';

function zeroCounts(): BuildStatusCounts {
  return { completed: 0, failed: 0, building: 0, queued: 0, substituted: 0, aborted: 0 };
}

function evalSummary(id: string, status: EvaluationSummary['status'] = 'Building'): EvaluationSummary {
  return {
    id,
    commit: 'abc1234def5678',
    commit_message: null,
    status,
    trigger: null,
    triggered_by: null,
    pr_number: null,
    total_builds: 0,
    builds: zeroCounts(),
    errors: 0,
    warnings: 0,
    created_at: '2026-01-01T00:00:00',
    updated_at: '2026-01-01T00:01:00',
  };
}

function activatedRouteStub(access: AccessState): ActivatedRoute {
  return {
    snapshot: {
      paramMap: convertToParamMap({ org: 'acme', project: 'demo' }),
      queryParamMap: convertToParamMap({}),
    },
    data: of({}),
    parent: { data: of({ projectAccess: { project: {}, access } }) },
  } as unknown as ActivatedRoute;
}

function projectFor(
  access: AccessState,
  extraEvals: EvaluationSummary[] = [],
  primaryStatus: EvaluationSummary['status'] = 'Building',
) {
  return {
    id: 'p',
    name: 'demo',
    display_name: 'Demo',
    description: '',
    repository: '',
    wildcard: '*',
    active: true,
    created_at: '2026-01-01T00:00:00',
    keep_evaluations: 5,
    last_check_at: '2026-01-01T00:00:00',
    queue: { building: 0, queued: 0 },
    last_evaluations: [evalSummary('e1', primaryStatus), ...extraEvals],
    can_edit: access.canEdit,
    can_trigger: access.canTrigger,
    managed: access.managed,
  };
}

function findByText(root: HTMLElement, text: string): HTMLElement | null {
  const target = text.toLowerCase();
  return (Array.from(root.querySelectorAll('button')) as HTMLElement[]).find(
    (el) => (el.textContent ?? '').trim().toLowerCase().includes(target),
  ) ?? null;
}

function makeProjectsService(access: AccessState, overrides: Partial<{
  startEvaluation: () => ReturnType<ProjectsService['startEvaluation']>;
  restartFailedBuilds: () => ReturnType<ProjectsService['restartFailedBuilds']>;
  abortEvaluation: (org: string, proj: string, id: string) => ReturnType<ProjectsService['abortEvaluation']>;
  getEntryPoints: () => ReturnType<ProjectsService['getEntryPoints']>;
  extraEvals: EvaluationSummary[];
  primaryStatus: EvaluationSummary['status'];
}> = {}): ProjectsService {
  const extraEvals = overrides.extraEvals ?? [];
  return {
    getProject: () => of(projectFor(access, extraEvals, overrides.primaryStatus)),
    getEntryPoints: overrides.getEntryPoints ?? (() => of([])),
    startEvaluation: overrides.startEvaluation ?? (() => of('ok')),
    restartFailedBuilds: overrides.restartFailedBuilds ?? (() => of('ok')),
    abortEvaluation: overrides.abortEvaluation ?? (() => of('ok')),
  } as unknown as ProjectsService;
}

function setup(
  access: AccessState,
  serviceOverrides: Parameters<typeof makeProjectsService>[1] = {},
): { fixture: ComponentFixture<ProjectDetailComponent>; projectsService: ProjectsService } {
  const projectsService = makeProjectsService(access, serviceOverrides);
  TestBed.configureTestingModule({
    imports: [ProjectDetailComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      { provide: ActivatedRoute, useValue: activatedRouteStub(access) },
      { provide: ProjectsService, useValue: projectsService },
      { provide: OrganizationsService, useValue: { getOrganization: () => of({ display_name: 'Acme' }) } },
      { provide: AuthService, useValue: { isAuthenticated: () => true } },
    ],
  });
  const fixture = TestBed.createComponent(ProjectDetailComponent);
  fixture.detectChanges();
  return { fixture, projectsService };
}

describe('ProjectDetailComponent - access gating', () => {
  it('hides Start Evaluation / Restart / Abort when canTrigger is false', () => {
    const { fixture } = setup({ managed: false, canEdit: false, canTrigger: false });
    expect(findByText(fixture.nativeElement, 'start evaluation')).toBeNull();
    expect(findByText(fixture.nativeElement, 'restart failed')).toBeNull();
    expect(findByText(fixture.nativeElement, 'abort')).toBeNull();
  });

  it('keeps Start Evaluation / Restart / Abort enabled on state-managed projects', () => {
    const { fixture } = setup({ managed: true, canEdit: true, canTrigger: true }, { primaryStatus: 'Completed' });
    const startBtn = findByText(fixture.nativeElement, 'start evaluation') as HTMLButtonElement | null;
    const restartBtn = findByText(fixture.nativeElement, 'restart failed') as HTMLButtonElement | null;
    expect(startBtn).not.toBeNull();
    expect(startBtn!.disabled).toBe(false);
    expect(restartBtn).not.toBeNull();
    expect(restartBtn!.disabled).toBe(false);
  });

  it('shows Start / Restart to a caller with TriggerEvaluation but not EditProject', () => {
    const { fixture } = setup({ managed: false, canEdit: false, canTrigger: true }, { primaryStatus: 'Completed' });
    const startBtn = findByText(fixture.nativeElement, 'start evaluation') as HTMLButtonElement | null;
    const restartBtn = findByText(fixture.nativeElement, 'restart failed') as HTMLButtonElement | null;
    expect(startBtn).not.toBeNull();
    expect(startBtn!.disabled).toBe(false);
    expect(restartBtn).not.toBeNull();
    expect(restartBtn!.disabled).toBe(false);
  });
});

describe('ProjectDetailComponent - in-progress state (#452)', () => {
  it('disables Start Evaluation while an evaluation is in progress', () => {
    const { fixture } = setup({ managed: false, canEdit: true, canTrigger: true }, { primaryStatus: 'Fetching' });
    const startBtn = findByText(fixture.nativeElement, 'start evaluation') as HTMLButtonElement | null;
    expect(startBtn).not.toBeNull();
    expect(startBtn!.disabled).toBe(true);
    expect(fixture.componentInstance.evaluationInProgress()).toBe(true);
  });

  it('enables Start Evaluation once the latest evaluation finished', () => {
    const { fixture } = setup({ managed: false, canEdit: true, canTrigger: true }, { primaryStatus: 'Completed' });
    const startBtn = findByText(fixture.nativeElement, 'start evaluation') as HTMLButtonElement | null;
    expect(startBtn!.disabled).toBe(false);
    expect(fixture.componentInstance.evaluationInProgress()).toBe(false);
  });

  it('shows the eval status badge in the title while in progress', () => {
    const { fixture } = setup({ managed: false, canEdit: true, canTrigger: true }, { primaryStatus: 'Building' });
    const badge = fixture.nativeElement.querySelector('.title-row app-eval-status-badge');
    expect(badge).toBeTruthy();
  });

  it('hides the title badge when the latest evaluation is terminal', () => {
    const { fixture } = setup({ managed: false, canEdit: true, canTrigger: true }, { primaryStatus: 'Completed' });
    expect(fixture.nativeElement.querySelector('.title-row app-eval-status-badge')).toBeNull();
  });
});

describe('ProjectDetailComponent - error surfacing (issue #280)', () => {
  it('shows an inline error banner when startEvaluation fails', () => {
    const msg = 'Failed to fetch repository state: connection refused';
    const { fixture } = setup({ managed: false, canEdit: true, canTrigger: true }, {
      startEvaluation: () => throwError(() => new Error(msg)),
    });
    fixture.componentInstance.startEvaluation();
    fixture.detectChanges();

    const banner = fixture.nativeElement.querySelector('.evaluation-error') as HTMLElement | null;
    expect(banner).not.toBeNull();
    expect(banner!.textContent).toContain('Failed to fetch repository state');
  });

  it('clears the error banner when the user retries', () => {
    const msg = 'Failed to fetch repository state: connection refused';
    const { fixture } = setup({ managed: false, canEdit: true, canTrigger: true }, {
      startEvaluation: () => throwError(() => new Error(msg)),
      restartFailedBuilds: () => throwError(() => new Error(msg)),
    });
    const component = fixture.componentInstance;
    component.startEvaluation();
    fixture.detectChanges();
    expect(component.errorMessage()).not.toBeNull();

    component.dismissError();
    fixture.detectChanges();
    expect(component.errorMessage()).toBeNull();
    expect(fixture.nativeElement.querySelector('.evaluation-error')).toBeNull();
  });
});

describe('ProjectDetailComponent - evaluation selection', () => {
  it('selecting an evaluation loads its entry points', () => {
    const e2 = evalSummary('e2', 'Completed');
    const { fixture, projectsService } = setup(
      { managed: false, canEdit: true, canTrigger: true },
      { extraEvals: [e2] },
    );
    const spy = vi.spyOn(projectsService, 'getEntryPoints').mockReturnValue(of([]));
    const component = fixture.componentInstance;
    component.select(component.evaluations()[1]);
    expect(spy).toHaveBeenCalledWith(component.orgName, component.projectName, component.evaluations()[1].id);
  });

  it('labels a pull-request trigger as "PR #<n>" (#391)', () => {
    const { fixture } = setup({ managed: false, canEdit: true, canTrigger: true });
    const component = fixture.componentInstance;
    const pr = { ...evalSummary('pr1'), trigger: { id: 't', type: 'reporter_pull_request' as const }, pr_number: 42 };
    expect(component.triggerLabel(pr)).toBe('PR #42');
    const noNum = { ...pr, pr_number: null };
    expect(component.triggerLabel(noNum)).toBe('PR');
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

    component.loadProjectData(false);
    expect(component.selected()?.id).toBe(component.evaluations()[1].id);
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
    expect(component.totalCount(component.barCounts(ep))).toBe(4);
    const sub = { deps: { ...deps, completed: 0, substituted: 0 }, build_status: 'Substituted' } as unknown as EntryPointSummary;
    expect(component.barCounts(sub).substituted).toBe(1);
  });
});

describe('ProjectDetailComponent - abort modal', () => {
  it('abort opens the confirm modal without immediately calling the service', () => {
    const { fixture, projectsService } = setup({ managed: false, canEdit: true, canTrigger: true });
    const spy = vi.spyOn(projectsService, 'abortEvaluation');
    fixture.componentInstance.abortTarget.set('e1');
    fixture.detectChanges();
    expect(document.querySelector('.p-dialog')).toBeTruthy();
    expect(spy).not.toHaveBeenCalled();
  });

  it('confirmAbort calls the service with the targeted evaluation id', () => {
    const { fixture, projectsService } = setup({ managed: false, canEdit: true, canTrigger: true });
    const spy = vi.spyOn(projectsService, 'abortEvaluation').mockReturnValue(of('Success'));
    const component = fixture.componentInstance;
    component.abortTarget.set('e1');
    component.confirmAbort();
    expect(spy).toHaveBeenCalledWith(component.orgName, component.projectName, 'e1');
  });
});
