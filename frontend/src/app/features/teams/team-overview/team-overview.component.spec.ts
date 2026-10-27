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
import { of } from 'rxjs';
import { TeamOverviewComponent } from './team-overview.component';
import { TeamsService } from '@core/services/teams.service';
import { AuthService } from '@core/services/auth.service';
import { ConfigService } from '@core/services/config.service';
import { TeamResponse } from '@core/models';

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

function render(): HTMLElement {
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
          get: () => of(team),
          members: () => of([]),
          invitations: () => of([]),
          workers: () => of([]),
          grants: () => of({ projects: [], caches: [] }),
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
  it('manages members and workers on the team page itself', () => {
    expect(headings(render())).toEqual(expect.arrayContaining(['Members', 'Workers']));
  });

  it('hides the projects and caches section when the team has no grants', () => {
    expect(headings(render())).not.toContain('Projects and Caches');
  });
});
