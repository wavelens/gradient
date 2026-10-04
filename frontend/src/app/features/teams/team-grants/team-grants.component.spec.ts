/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { of } from 'rxjs';
import { TeamGrantsComponent } from './team-grants.component';
import { TeamsService } from '@core/services/teams.service';
import { TeamGrant } from '@core/models';

const grants: TeamGrant[] = [
  { team: 'platform', display_name: 'Platform', role: 'Write', users: true, workers: true, pending: false },
  { team: 'ops', display_name: 'Ops', role: null, users: false, workers: true, pending: true },
];

function setup(kind: 'project' | 'cache') {
  const grantProject = vi.fn().mockReturnValue(of('Request sent'));
  const grantCache = vi.fn().mockReturnValue(of('Team granted'));
  TestBed.configureTestingModule({
    imports: [TeamGrantsComponent],
    providers: [
      provideRouter([]),
      {
        provide: TeamsService,
        useValue: {
          list: () => of([]),
          projectGrants: () => of(grants),
          cacheGrants: () => of(grants),
          grantProject,
          grantCache,
          removeProjectGrant: vi.fn().mockReturnValue(of('ok')),
          removeCacheGrant: vi.fn().mockReturnValue(of('ok')),
          updateProjectGrant: vi.fn().mockReturnValue(of('ok')),
          updateCacheGrant: vi.fn().mockReturnValue(of('ok')),
        },
      },
    ],
  });
  const fixture = TestBed.createComponent(TeamGrantsComponent);
  fixture.componentRef.setInput('kind', kind);
  fixture.componentRef.setInput('name', 'acme');
  fixture.componentRef.setInput('roles', ['Admin', 'Write', 'View']);
  fixture.componentRef.setInput('canEdit', true);
  fixture.detectChanges();
  return { fixture, grantProject, grantCache };
}

describe('TeamGrantsComponent', () => {
  it('marks a grant that waits for the team as pending', () => {
    const { fixture } = setup('project');
    const text = (fixture.nativeElement as HTMLElement).textContent || '';
    expect(text).toContain('Platform');
    expect(text).toContain('Pending');
  });

  it('grants a project the chosen parts of a team', () => {
    const { fixture, grantProject } = setup('project');
    const component = fixture.componentInstance;
    component.form = { team: 'platform', role: 'View', users: false, workers: true };
    component.grant();
    expect(grantProject).toHaveBeenCalledWith('acme', { team: 'platform', role: undefined, users: false, workers: true });
  });

  it('grants a cache only users with a role', () => {
    const { fixture, grantCache } = setup('cache');
    const component = fixture.componentInstance;
    component.form = { team: 'platform', role: 'View', users: true, workers: false };
    component.grant();
    expect(grantCache).toHaveBeenCalledWith('acme', 'platform', 'View');
  });
});
