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
import { ProjectSettingsComponent } from './project-settings.component';
import { ProjectsService } from '@core/services/projects.service';
import { OrganizationsService } from '@core/services/organizations.service';
import { IntegrationsService } from '@core/services/integrations.service';
import { AccessState } from '@core/models/access.model';

type AccessCase = { managed: boolean; canEdit: boolean; canTrigger?: boolean };

function asAccess(c: AccessCase): AccessState {
  return { managed: c.managed, canEdit: c.canEdit, canTrigger: c.canTrigger ?? c.canEdit };
}

function projectFor(c: AccessCase) {
  return {
    id: 'p',
    organization: 'acme',
    name: 'demo',
    display_name: 'Demo',
    description: '',
    repository: '',
    wildcard: '',
    active: true,
    force_evaluation: false,
    keep_evaluations: 30,
    concurrency: 'soft_abort' as const,
    sign_cache: true,
    managed: c.managed,
    can_edit: c.canEdit,
    can_trigger: c.canTrigger ?? c.canEdit,
  };
}

function activatedRouteStub(c: AccessCase): ActivatedRoute {
  return {
    snapshot: { paramMap: convertToParamMap({ org: 'acme', project: 'demo' }) },
    data: of({}),
    parent: { data: of({ projectAccess: { project: projectFor(c), access: asAccess(c) } }) },
  } as unknown as ActivatedRoute;
}

function findByText(root: HTMLElement, text: string): HTMLElement | null {
  const target = text.toLowerCase();
  return (Array.from(root.querySelectorAll('button')) as HTMLElement[]).find(
    (el) => (el.textContent ?? '').trim().toLowerCase().includes(target),
  ) ?? null;
}

function setup(c: AccessCase): ComponentFixture<ProjectSettingsComponent> {
  TestBed.configureTestingModule({
    imports: [ProjectSettingsComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      { provide: ActivatedRoute, useValue: activatedRouteStub(c) },
      {
        provide: ProjectsService,
        useValue: { getProjectInfo: () => of(projectFor(c)) },
      },
      {
        provide: OrganizationsService,
        useValue: { getOrganization: () => of({ id: 'o', display_name: 'Acme' }) },
      },
      {
        provide: IntegrationsService,
        useValue: {
          listOrgIntegrations: () => of([]),
          getProjectIntegration: () => of(null),
        },
      },
    ],
  });
  const fixture = TestBed.createComponent(ProjectSettingsComponent);
  fixture.detectChanges();
  return fixture;
}

describe('ProjectSettingsComponent - access gating', () => {
  it('hides Save Changes when access is read-only', () => {
    const fixture = setup({ managed: false, canEdit: false, canTrigger: false });
    expect(findByText(fixture.nativeElement, 'save changes')).toBeNull();
  });

  it('shows but disables Save Changes when project is state-managed', () => {
    const fixture = setup({ managed: true, canEdit: true, canTrigger: true });
    const btn = findByText(fixture.nativeElement, 'save changes') as HTMLButtonElement | null;
    expect(btn).not.toBeNull();
    expect(btn!.disabled).toBe(true);
  });

  it('hides Delete Project under read-only access', () => {
    const fixture = setup({ managed: false, canEdit: false, canTrigger: false });
    expect(findByText(fixture.nativeElement, 'delete project')).toBeNull();
  });

  it('shows but disables Delete Project when managed', () => {
    const fixture = setup({ managed: true, canEdit: true, canTrigger: true });
    const btn = findByText(fixture.nativeElement, 'delete project') as HTMLButtonElement | null;
    expect(btn).not.toBeNull();
    expect(btn!.disabled).toBe(true);
  });
});
