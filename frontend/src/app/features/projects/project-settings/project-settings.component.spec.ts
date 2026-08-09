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
import { ProjectAccessService } from '@core/services/project-access.service';
import { AccessState } from '@core/models/access.model';
import { Project } from '@core/models/project.model';

function projectFor(access: AccessState): Project {
  return {
    id: 'o',
    name: 'acme',
    display_name: 'Acme',
    description: '',
    public: false,
    hide_build_requests: false,
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

function setup(access: AccessState): ComponentFixture<ProjectSettingsComponent> {
  TestBed.configureTestingModule({
    imports: [ProjectSettingsComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      {
        provide: ActivatedRoute,
        useValue: { snapshot: { paramMap: convertToParamMap({ project: 'acme' }) } },
      },
      {
        provide: ProjectsService,
        useValue: {
          getProject: () => of(projectFor(access)),
          getSSHKey: () => of(''),
        },
      },
      { provide: ProjectAccessService, useValue: { forProject: () => Promise.resolve(access) } },
    ],
  });
  const fixture = TestBed.createComponent(ProjectSettingsComponent);
  fixture.detectChanges();
  return fixture;
}

async function settled(fixture: ComponentFixture<ProjectSettingsComponent>) {
  await fixture.whenStable();
  fixture.detectChanges();
}

describe('ProjectSettingsComponent - access gating', () => {
  it('hides Save / Delete under read-only access', async () => {
    const fixture = setup({ managed: false, canEdit: false, canTrigger: false });
    await settled(fixture);
    expect(findByText(fixture.nativeElement, 'save changes')).toBeNull();
    expect(findByText(fixture.nativeElement, 'delete project')).toBeNull();
  });

  it('shows but disables Save / Delete under state-managed access', async () => {
    const fixture = setup({ managed: true, canEdit: true, canTrigger: true });
    await settled(fixture);
    const save = findByText(fixture.nativeElement, 'save changes') as HTMLButtonElement | null;
    const del = findByText(fixture.nativeElement, 'delete project') as HTMLButtonElement | null;
    expect(save).not.toBeNull();
    expect(save!.disabled).toBe(true);
    expect(del).not.toBeNull();
    expect(del!.disabled).toBe(true);
  });
});
