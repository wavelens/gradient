/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { of } from 'rxjs';
import { TeamOverviewComponent } from './team-overview.component';
import { TeamsService } from '@core/services/teams.service';
import { AuthService } from '@core/services/auth.service';
import { TeamResponse, TeamEvaluation, TeamWorkerEntry } from '@core/models';

const team: TeamResponse = {
  id: 't1',
  name: 'platform',
  display_name: 'Platform',
  managed: false,
  role: 'member',
  oidc_group: null,
  scim_group: null,
  new_project_users: false,
  new_project_workers: false,
  new_project_role: null,
};

const evaluations: TeamEvaluation[] = [
  { id: 'e1', project: 'web', task: 'app', status: 'Completed', created_at: '2026-10-04T10:00:00' },
];

function render(workers: TeamWorkerEntry[] = []): HTMLElement {
  TestBed.configureTestingModule({
    imports: [TeamOverviewComponent],
    providers: [
      provideRouter([]),
      { provide: ActivatedRoute, useValue: { snapshot: { paramMap: convertToParamMap({ team: 'platform' }) } } },
      {
        provide: TeamsService,
        useValue: {
          get: () => of(team),
          members: () => of([]),
          workers: () => of(workers),
          grants: () => of({ projects: [], caches: [] }),
          evaluations: () => of(evaluations),
        },
      },
      { provide: AuthService, useValue: { user: signal({ superuser: false }) } },
    ],
  });
  const fixture = TestBed.createComponent(TeamOverviewComponent);
  fixture.detectChanges();
  return fixture.nativeElement as HTMLElement;
}

function headings(element: HTMLElement): string[] {
  return Array.from(element.querySelectorAll('h2'), (heading) => heading.textContent?.trim() ?? '');
}

describe('TeamOverviewComponent', () => {
  it('links the recent evaluations of granted projects', () => {
    const element = render();
    expect(element.textContent).toContain('Recent Evaluations');
    expect(element.querySelector('a[href="/project/web/log/e1"]')).not.toBeNull();
  });

  it('hides the workers section when the team has no workers', () => {
    expect(headings(render())).not.toContain('Workers');
  });

  it('hides the projects and caches section when the team has no grants', () => {
    expect(headings(render())).not.toContain('Projects and Caches');
  });

  it('lists the team workers', () => {
    const worker = { worker_id: 'w1', display_name: 'Builder', active: true, connected: true } as TeamWorkerEntry;
    const element = render([worker]);
    expect(headings(element)).toContain('Workers');
    expect(element.textContent).toContain('Builder');
  });
});
