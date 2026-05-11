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
import { OrganizationSettingsComponent } from './organization-settings.component';
import { OrganizationsService } from '@core/services/organizations.service';
import { OrgAccessService } from '@core/services/org-access.service';
import { AccessState } from '@core/models/access.model';
import { Organization } from '@core/models/organization.model';

function orgFor(access: AccessState): Organization {
  return {
    id: 'o',
    name: 'acme',
    display_name: 'Acme',
    description: '',
    public: false,
    managed: access.managed,
    role: access.canEdit ? 'Admin' : 'View',
  };
}

function findByText(root: HTMLElement, text: string): HTMLElement | null {
  const target = text.toLowerCase();
  return (Array.from(root.querySelectorAll('button')) as HTMLElement[]).find(
    (el) => (el.textContent ?? '').trim().toLowerCase().includes(target),
  ) ?? null;
}

function setup(access: AccessState): ComponentFixture<OrganizationSettingsComponent> {
  TestBed.configureTestingModule({
    imports: [OrganizationSettingsComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      {
        provide: ActivatedRoute,
        useValue: { snapshot: { paramMap: convertToParamMap({ org: 'acme' }) } },
      },
      {
        provide: OrganizationsService,
        useValue: {
          getOrganization: () => of(orgFor(access)),
          getSSHKey: () => of(''),
        },
      },
      { provide: OrgAccessService, useValue: { forOrg: () => Promise.resolve(access) } },
    ],
  });
  const fixture = TestBed.createComponent(OrganizationSettingsComponent);
  fixture.detectChanges();
  return fixture;
}

async function settled(fixture: ComponentFixture<OrganizationSettingsComponent>) {
  await fixture.whenStable();
  fixture.detectChanges();
}

describe('OrganizationSettingsComponent — access gating', () => {
  it('hides Save / Delete under read-only access', async () => {
    const fixture = setup({ managed: false, canEdit: false, canTrigger: false });
    await settled(fixture);
    expect(findByText(fixture.nativeElement, 'save changes')).toBeNull();
    expect(findByText(fixture.nativeElement, 'delete organization')).toBeNull();
  });

  it('shows but disables Save / Delete under state-managed access', async () => {
    const fixture = setup({ managed: true, canEdit: true, canTrigger: true });
    await settled(fixture);
    const save = findByText(fixture.nativeElement, 'save changes') as HTMLButtonElement | null;
    const del = findByText(fixture.nativeElement, 'delete organization') as HTMLButtonElement | null;
    expect(save).not.toBeNull();
    expect(save!.disabled).toBe(true);
    expect(del).not.toBeNull();
    expect(del!.disabled).toBe(true);
  });
});
