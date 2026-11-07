/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { of, throwError } from 'rxjs';
import { TeamOverviewComponent } from './team-overview.component';
import { TeamsService } from '@core/services/teams.service';
import { AuthService } from '@core/services/auth.service';
import { ConfigService } from '@core/services/config.service';
import { NotifyService } from '@core/services/notify.service';
import { ApiError } from '@core/services/api.service';
import { TeamRequestItem, TeamResponse } from '@core/models';

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

const request: TeamRequestItem = {
  id: 'r1',
  kind: 'project',
  target: 'acme',
  display_name: 'Acme',
  users: true,
  workers: false,
  role: 'View',
  created_at: '2026-10-01T10:00:00',
};

function setup(role: TeamResponse['role'] = 'member', approveRequest = vi.fn().mockReturnValue(of('ok'))) {
  const notify = { success: vi.fn(), failure: vi.fn() };
  TestBed.configureTestingModule({
    imports: [TeamOverviewComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      { provide: ActivatedRoute, useValue: { snapshot: { paramMap: convertToParamMap({ team: 'platform' }) } } },
      { provide: ConfigService, useValue: { gradientCiEnabled: false } },
      {
        provide: TeamsService,
        useValue: {
          get: () => of({ ...team, role }),
          members: () => of([]),
          invitations: () => of([]),
          workers: () => of([]),
          grants: () => of({ projects: [], caches: [] }),
          requests: () => of([request]),
          approveRequest,
        },
      },
      { provide: AuthService, useValue: { user: signal({ superuser: false }) } },
      { provide: NotifyService, useValue: notify },
    ],
  });
  const fixture = TestBed.createComponent(TeamOverviewComponent);
  fixture.detectChanges();
  return { fixture, notify, root: fixture.nativeElement as HTMLElement };
}

function render(): HTMLElement {
  return setup().root;
}

function headings(element: HTMLElement): string[] {
  return Array.from(element.querySelectorAll('h2'), (heading) => heading.textContent?.trim() ?? '');
}

describe('TeamOverviewComponent', () => {
  it('manages members and workers on the team page itself', () => {
    expect(headings(render())).toEqual(expect.arrayContaining(['Members', 'Workers']));
  });

  it('hides the projects and caches section when the team has no grants', () => {
    expect(headings(render())).not.toContain('Projects and Caches');
  });

  it('reports a failed approval as a toast and keeps the page usable', () => {
    const failure = new ApiError('Project no longer exists', 404);
    const { fixture, notify, root } = setup('admin', vi.fn().mockReturnValue(throwError(() => failure)));
    fixture.componentInstance.approve('r1');
    fixture.detectChanges();
    expect(notify.failure).toHaveBeenCalledWith('Failed to approve request', failure);
    expect(fixture.componentInstance.busy()).toBeNull();
    expect(root.textContent).not.toContain('Project no longer exists');
  });

  it('confirms an approved request', () => {
    const { fixture, notify } = setup('admin');
    fixture.componentInstance.approve('r1');
    expect(notify.success).toHaveBeenCalledWith('Request approved');
  });
});
