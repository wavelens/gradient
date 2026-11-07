/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { of, throwError } from 'rxjs';
import { ProjectSettingsComponent } from './project-settings.component';
import { ProjectsService } from '@core/services/projects.service';
import { ProjectAccessService } from '@core/services/project-access.service';
import { AccessState } from '@core/models/access.model';
import { ProjectResponse } from '@core/models';

function projectFor(access: AccessState): ProjectResponse {
  return {
    id: 'o',
    name: 'acme',
    display_name: 'Acme',
    description: '',
    public: false,
    hide_build_requests: false,
    allow_exec_in_build_requests: false,
    managed: access.managed,
    created_by: 'u1',
    created_at: '2026-01-01T00:00:00',
    github_app_available: false,
    role: access.canEdit ? 'Admin' : 'View',
  };
}

function findByText(root: HTMLElement, text: string): HTMLElement | null {
  const target = text.toLowerCase();
  return (Array.from(root.querySelectorAll('button')) as HTMLElement[]).find(
    (el) => (el.textContent ?? '').trim().toLowerCase().includes(target),
  ) ?? null;
}

function setup(access: AccessState):ComponentFixture<ProjectSettingsComponent> {
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
          getSSHKey: () => of('ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEd25519 acme'),
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

describe('ProjectSettingsComponent - SSH key load errors', () => {
  it('shows a failed key load as an error instead of offering to generate a key', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    await settled(fixture);
    const service = TestBed.inject(ProjectsService) as unknown as { getSSHKey: () => unknown };
    service.getSSHKey = () => throwError(() => new Error('database is down'));
    fixture.componentInstance.loadSSHKey();
    fixture.detectChanges();
    const root = fixture.nativeElement as HTMLElement;
    expect(root.textContent).toContain('Failed to load SSH key');
    expect(root.textContent).not.toContain('No SSH key generated yet.');

    service.getSSHKey = () => of('ssh-ed25519 AAAA acme');
    root.querySelector<HTMLButtonElement>('.gr-load-state__error button')!.click();
    fixture.detectChanges();
    expect(root.textContent).not.toContain('Failed to load SSH key');
  });
});

describe('ProjectSettingsComponent - project load errors', () => {
  it('shows a failed project load with Try Again instead of a blank page', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    await settled(fixture);
    const service = TestBed.inject(ProjectsService) as unknown as { getProject: () => unknown };
    service.getProject = () => throwError(() => new Error('database is down'));
    fixture.componentInstance.loadProject();
    fixture.detectChanges();
    const root = fixture.nativeElement as HTMLElement;
    expect(root.textContent).toContain('Failed to load project settings');
    expect(root.textContent).toContain('database is down');
  });
});
