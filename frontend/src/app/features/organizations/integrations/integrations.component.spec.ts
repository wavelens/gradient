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
import { IntegrationsComponent } from './integrations.component';
import { IntegrationsService } from '@core/services/integrations.service';
import { OrganizationsService } from '@core/services/organizations.service';
import { OrgAccessService } from '@core/services/org-access.service';
import { AccessState, Integration } from '@core/models';

const baseIntegration: Integration = {
  id: 'i1',
  organization: 'o',
  name: 'gitea-prod',
  display_name: 'Gitea Prod',
  kind: 'inbound',
  forge_type: 'gitea',
  endpoint_url: null,
  has_secret: true,
  has_access_token: false,
  created_by: 'u',
  created_at: '2026-01-01T00:00:00Z',
};

function activatedRouteStub() {
  return {
    snapshot: { paramMap: convertToParamMap({ org: 'acme' }) },
  } as Partial<ActivatedRoute>;
}

function setup(access: AccessState, integrations: Integration[]): ComponentFixture<IntegrationsComponent> {
  TestBed.configureTestingModule({
    imports: [IntegrationsComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      { provide: ActivatedRoute, useValue: activatedRouteStub() },
      {
        provide: IntegrationsService,
        useValue: {
          listOrgIntegrations: () => of(integrations),
        },
      },
      {
        provide: OrganizationsService,
        useValue: {
          getOrganization: () =>
            of({ id: 'o', display_name: 'Acme', github_app_available: false }),
        },
      },
      { provide: OrgAccessService, useValue: { forOrg: () => Promise.resolve(access) } },
    ],
  });
  return TestBed.createComponent(IntegrationsComponent);
}

async function settled(fixture: ComponentFixture<IntegrationsComponent>) {
  fixture.detectChanges();
  await fixture.whenStable();
  fixture.detectChanges();
}

function findByText(root: HTMLElement, text: string): HTMLElement | null {
  const target = text.toLowerCase();
  return (Array.from(root.querySelectorAll('button')) as HTMLElement[]).find(
    (el) => (el.textContent ?? '').trim().toLowerCase().includes(target),
  ) ?? null;
}

function findAllByText(root: HTMLElement, text: string): HTMLButtonElement[] {
  const target = text.toLowerCase();
  return (Array.from(root.querySelectorAll('button')) as HTMLButtonElement[]).filter(
    (el) => (el.textContent ?? '').trim().toLowerCase().includes(target),
  );
}

describe('IntegrationsComponent - access gating', () => {
  it('hides New Integration / Edit / Delete under read-only access', async () => {
    const fixture = setup({ managed: false, canEdit: false, canTrigger: false }, [baseIntegration]);
    await settled(fixture);
    expect(findByText(fixture.nativeElement, 'new integration')).toBeNull();
    expect(findByText(fixture.nativeElement, 'edit')).toBeNull();
    expect(findByText(fixture.nativeElement, 'delete')).toBeNull();
  });

  it('shows but disables New Integration / Edit / Delete under state-managed access', async () => {
    const fixture = setup({ managed: true, canEdit: true, canTrigger: true }, [baseIntegration]);
    await settled(fixture);
    const newBtn = findByText(fixture.nativeElement, 'new integration') as HTMLButtonElement | null;
    expect(newBtn).not.toBeNull();
    expect(newBtn!.disabled).toBe(true);

    const editButtons = findAllByText(fixture.nativeElement, 'edit');
    const deleteButtons = findAllByText(fixture.nativeElement, 'delete');
    expect(editButtons.length).toBeGreaterThan(0);
    expect(deleteButtons.length).toBeGreaterThan(0);
    expect(editButtons.every((b) => b.disabled)).toBe(true);
    expect(deleteButtons.every((b) => b.disabled)).toBe(true);
  });

  it('shows working New Integration / Edit / Delete under full access', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true }, [baseIntegration]);
    await settled(fixture);
    const newBtn = findByText(fixture.nativeElement, 'new integration') as HTMLButtonElement | null;
    expect(newBtn).not.toBeNull();
    expect(newBtn!.disabled).toBe(false);
    expect(findByText(fixture.nativeElement, 'edit')).not.toBeNull();
    expect(findByText(fixture.nativeElement, 'delete')).not.toBeNull();
  });
});
