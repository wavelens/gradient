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
import { ProjectTriggersComponent } from './project-triggers.component';
import { TriggersService } from '@core/services/triggers.service';
import { IntegrationsService } from '@core/services/integrations.service';
import { OrganizationsService } from '@core/services/organizations.service';
import { AccessState } from '@core/models/access.model';

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

const trigger = {
  id: 't1',
  type: 'polling' as const,
  active: true,
  config: { type: 'polling', interval_secs: 300, branch: null },
  last_fired_at: null,
};

function setup(access: AccessState): ComponentFixture<ProjectTriggersComponent> {
  TestBed.configureTestingModule({
    imports: [ProjectTriggersComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      { provide: ActivatedRoute, useValue: activatedRouteStub(access) },
      { provide: TriggersService, useValue: { list: () => of([trigger]) } },
      { provide: IntegrationsService, useValue: { listOrgIntegrations: () => of([]) } },
      { provide: OrganizationsService, useValue: { getOrganization: () => of({ display_name: 'Acme' }) } },
    ],
  });
  const fixture = TestBed.createComponent(ProjectTriggersComponent);
  fixture.detectChanges();
  return fixture;
}

describe('ProjectTriggersComponent — access gating', () => {
  it('hides New Trigger / Edit / Delete / Fire Now buttons under read-only', () => {
    const fixture = setup({ managed: false, canEdit: false });
    expect(findByText(fixture.nativeElement, 'new trigger')).toBeNull();
    expect(findByText(fixture.nativeElement, 'edit')).toBeNull();
    expect(findByText(fixture.nativeElement, 'delete')).toBeNull();
    expect(findByText(fixture.nativeElement, 'fire now')).toBeNull();
  });

  it('shows but disables write buttons under state-managed access', () => {
    const fixture = setup({ managed: true, canEdit: true });
    const newBtn = findByText(fixture.nativeElement, 'new trigger') as HTMLButtonElement | null;
    const editBtn = findByText(fixture.nativeElement, 'edit') as HTMLButtonElement | null;
    const deleteBtn = findByText(fixture.nativeElement, 'delete') as HTMLButtonElement | null;
    const fireBtn = findByText(fixture.nativeElement, 'fire now') as HTMLButtonElement | null;
    expect(newBtn).not.toBeNull();
    expect(newBtn!.disabled).toBe(true);
    expect(editBtn).not.toBeNull();
    expect(editBtn!.disabled).toBe(true);
    expect(deleteBtn).not.toBeNull();
    expect(deleteBtn!.disabled).toBe(true);
    expect(fireBtn).not.toBeNull();
    expect(fireBtn!.disabled).toBe(true);
  });
});
