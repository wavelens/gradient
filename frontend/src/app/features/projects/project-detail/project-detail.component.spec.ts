/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { of } from 'rxjs';
import { ProjectDetailComponent } from './project-detail.component';
import { ProjectsService } from '@core/services/projects.service';
import { OrganizationsService } from '@core/services/organizations.service';
import { AuthService } from '@core/services/auth.service';
import { AccessState } from '@core/models/access.model';

function activatedRouteStub(access: AccessState): ActivatedRoute {
  return {
    snapshot: { paramMap: convertToParamMap({ org: 'acme', project: 'demo' }) },
    data: of({}),
    parent: { data: of({ projectAccess: { project: {}, access } }) },
  } as unknown as ActivatedRoute;
}

function projectFor(access: AccessState) {
  return {
    id: 'p',
    name: 'demo',
    display_name: 'Demo',
    description: '',
    repository: '',
    wildcard: '*',
    active: true,
    created_at: '',
    keep_evaluations: 5,
    last_evaluations: [
      {
        id: 'e1',
        commit: 'abc',
        status: 'Building' as const,
        trigger: null,
        total_builds: 0,
        failed_builds: 0,
        completed_entry_points: 0,
        failed_entry_points: 0,
        entry_point_diff: null,
        created_at: '2026-01-01T00:00:00',
        updated_at: '2026-01-01T00:01:00',
      },
    ],
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

function setup(access: AccessState): ComponentFixture<ProjectDetailComponent> {
  TestBed.configureTestingModule({
    imports: [ProjectDetailComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      { provide: ActivatedRoute, useValue: activatedRouteStub(access) },
      {
        provide: ProjectsService,
        useValue: {
          getProject: () => of(projectFor(access)),
          getEntryPoints: () => of([]),
        },
      },
      { provide: OrganizationsService, useValue: { getOrganization: () => of({ display_name: 'Acme' }) } },
      { provide: AuthService, useValue: { isAuthenticated: () => true } },
    ],
  });
  const fixture = TestBed.createComponent(ProjectDetailComponent);
  fixture.detectChanges();
  return fixture;
}

describe('ProjectDetailComponent — access gating', () => {
  it('hides Start Evaluation / Restart / Abort when canTrigger is false', () => {
    const fixture = setup({ managed: false, canEdit: false, canTrigger: false });
    expect(findByText(fixture.nativeElement, 'start evaluation')).toBeNull();
    expect(findByText(fixture.nativeElement, 'restart failed')).toBeNull();
    expect(findByText(fixture.nativeElement, 'abort')).toBeNull();
  });

  it('keeps Start Evaluation / Restart / Abort enabled on state-managed projects', () => {
    // Backend permits TriggerEvaluation on managed projects (reject_managed=false),
    // so AccessService.triggerAccess strips the managed flag — buttons stay live.
    const fixture = setup({ managed: true, canEdit: true, canTrigger: true });
    const startBtn = findByText(fixture.nativeElement, 'start evaluation') as HTMLButtonElement | null;
    const restartBtn = findByText(fixture.nativeElement, 'restart failed') as HTMLButtonElement | null;
    const abortBtn = findByText(fixture.nativeElement, 'abort') as HTMLButtonElement | null;
    expect(startBtn).not.toBeNull();
    expect(startBtn!.disabled).toBe(false);
    expect(restartBtn).not.toBeNull();
    expect(restartBtn!.disabled).toBe(false);
    expect(abortBtn).not.toBeNull();
    expect(abortBtn!.disabled).toBe(false);
  });

  it('shows Start / Restart / Abort to a caller with TriggerEvaluation but not EditProject', () => {
    // The model split exists so this caller can act; they should NOT see
    // config-edit affordances (Settings appears via authService.isAuthenticated()
    // and only navigates) but trigger buttons must render and be enabled.
    const fixture = setup({ managed: false, canEdit: false, canTrigger: true });
    const startBtn = findByText(fixture.nativeElement, 'start evaluation') as HTMLButtonElement | null;
    const restartBtn = findByText(fixture.nativeElement, 'restart failed') as HTMLButtonElement | null;
    expect(startBtn).not.toBeNull();
    expect(startBtn!.disabled).toBe(false);
    expect(restartBtn).not.toBeNull();
    expect(restartBtn!.disabled).toBe(false);
  });
});
