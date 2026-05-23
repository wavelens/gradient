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
import { ProjectActionsComponent } from './project-actions.component';
import { ActionsService } from '@core/services/actions.service';
import { IntegrationsService } from '@core/services/integrations.service';
import { OrganizationsService } from '@core/services/organizations.service';
import { ConfigService } from '@core/services/config.service';
import { AccessState } from '@core/models/access.model';
import { Action } from '@core/models';

function activatedRouteStub(access: AccessState): ActivatedRoute {
  return {
    snapshot: { paramMap: convertToParamMap({ org: 'acme', project: 'demo' }) },
    data: of({}),
    parent: { data: of({ projectAccess: { project: {}, access } }) },
  } as unknown as ActivatedRoute;
}

function findByText(root: HTMLElement, text: string): HTMLElement | null {
  const target = text.toLowerCase();
  return (Array.from(root.querySelectorAll('button')) as HTMLElement[]).find(
    (el) => (el.textContent ?? '').trim().toLowerCase().includes(target),
  ) ?? null;
}

const action: Action = {
  id: 'a-1',
  name: 'Notify Ops',
  action_type: 'send_mail',
  config: { type: 'send_mail', recipients: ['ops@example.com'] },
  events: ['build.failed'],
  active: true,
  last_fired_at: null,
  created_by: 'u',
  created_at: '2026-05-01T00:00:00Z',
  updated_at: '2026-05-01T00:00:00Z',
};

function setup(
  access: AccessState,
  actions: Action[] = [action],
): ComponentFixture<ProjectActionsComponent> {
  TestBed.configureTestingModule({
    imports: [ProjectActionsComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      { provide: ActivatedRoute, useValue: activatedRouteStub(access) },
      {
        provide: ActionsService,
        useValue: {
          list: () => of(actions),
          create: () => of({ action, token: undefined }),
          update: () => of(action),
          delete: () => of({ deleted: true }),
          test: () => of(undefined),
        },
      },
      { provide: IntegrationsService, useValue: { listOrgIntegrations: () => of([]) } },
      { provide: OrganizationsService, useValue: { getOrganization: () => of({ display_name: 'Acme' }) } },
      { provide: ConfigService, useValue: { smtpEnabled: true } },
    ],
  });
  const fixture = TestBed.createComponent(ProjectActionsComponent);
  fixture.detectChanges();
  return fixture;
}

describe('ProjectActionsComponent', () => {
  it('hides New Action / Edit / Delete / Test buttons under read-only', () => {
    const fixture = setup({ managed: false, canEdit: false, canTrigger: false });
    expect(findByText(fixture.nativeElement, 'new action')).toBeNull();
    expect(findByText(fixture.nativeElement, 'edit')).toBeNull();
    expect(findByText(fixture.nativeElement, 'delete')).toBeNull();
    expect(findByText(fixture.nativeElement, 'test')).toBeNull();
  });

  it('shows but disables write buttons under state-managed access', () => {
    const fixture = setup({ managed: true, canEdit: true, canTrigger: true });
    const newBtn = findByText(fixture.nativeElement, 'new action') as HTMLButtonElement | null;
    const editBtn = findByText(fixture.nativeElement, 'edit') as HTMLButtonElement | null;
    const deleteBtn = findByText(fixture.nativeElement, 'delete') as HTMLButtonElement | null;
    const testBtn = findByText(fixture.nativeElement, 'test') as HTMLButtonElement | null;
    expect(newBtn).not.toBeNull();
    expect(newBtn!.disabled).toBe(true);
    expect(editBtn).not.toBeNull();
    expect(editBtn!.disabled).toBe(true);
    expect(deleteBtn).not.toBeNull();
    expect(deleteBtn!.disabled).toBe(true);
    expect(testBtn).not.toBeNull();
    expect(testBtn!.disabled).toBe(true);
  });

  it('renders action name and event chips', () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    const text = fixture.nativeElement.textContent ?? '';
    expect(text).toContain('Notify Ops');
    expect(text).toContain('build.failed');
  });

  it('reveals token after create when response includes one', () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    const comp = fixture.componentInstance;
    (comp as any).actionsService = { create: () => of({ action, token: 'gat_secret' }), list: () => of([action]) };
    comp.onCreateSaved({ name: 'X', config: { type: 'send_web_request', url: 'u' } } as any);
    expect(comp.revealedToken()).toBe('gat_secret');
  });
});
