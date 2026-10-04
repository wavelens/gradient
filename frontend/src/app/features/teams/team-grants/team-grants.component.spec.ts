/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { of, throwError } from 'rxjs';
import { GrantedPart, TeamGrantsComponent } from './team-grants.component';
import { TeamsService } from '@core/services/teams.service';
import { TeamGrant } from '@core/models';

const grants: TeamGrant[] = [
  { team: 'platform', display_name: 'Platform', role: 'Write', users: true, workers: true, pending: false },
  { team: 'docs', display_name: 'Docs', role: 'View', users: true, workers: false, pending: false },
  { team: 'ops', display_name: 'Ops', role: null, users: false, workers: true, pending: false },
  { team: 'qa', display_name: 'QA', role: null, users: false, workers: true, pending: true },
];

function setup(kind: 'project' | 'cache', part?: GrantedPart) {
  const service = {
    list: () => of([]),
    projectGrants: () => of(grants),
    cacheGrants: () => of(grants.filter((g) => g.users)),
    grantProject: vi.fn().mockReturnValue(of('Team granted')),
    grantCache: vi.fn().mockReturnValue(of('Team granted')),
    removeProjectGrant: vi.fn().mockReturnValue(of('ok')),
    removeCacheGrant: vi.fn().mockReturnValue(of('ok')),
    updateProjectGrant: vi.fn().mockReturnValue(of('ok')),
    updateCacheGrant: vi.fn().mockReturnValue(of('ok')),
  };
  TestBed.configureTestingModule({
    imports: [TeamGrantsComponent],
    providers: [provideRouter([]), { provide: TeamsService, useValue: service }],
  });
  const fixture = TestBed.createComponent(TeamGrantsComponent);
  fixture.componentRef.setInput('kind', kind);
  fixture.componentRef.setInput('name', 'acme');
  if (part) fixture.componentRef.setInput('grants', part);
  fixture.componentRef.setInput('roles', ['Admin', 'Write', 'View']);
  fixture.componentRef.setInput('canEdit', true);
  fixture.detectChanges();
  const changed = vi.fn();
  fixture.componentInstance.changed.subscribe(changed);
  const text = () => (fixture.nativeElement as HTMLElement).textContent || '';
  return { fixture, component: fixture.componentInstance, service, text, changed };
}

function grantOf(team: string): TeamGrant {
  return grants.find((g) => g.team === team)!;
}

describe('TeamGrantsComponent', () => {
  describe('for users', () => {
    it('lists only the grants that include users, with their role', () => {
      const { text } = setup('project', 'users');
      expect(text()).toContain('Platform');
      expect(text()).toContain('Role View');
      expect(text()).not.toContain('Ops');
      expect(text()).not.toContain('QA');
    });

    it('creates a users-only grant for a team without one', () => {
      const { component, service } = setup('project', 'users');
      component.form = { team: 'newteam', role: 'View' };
      component.grant();
      expect(service.grantProject).toHaveBeenCalledWith('acme', { team: 'newteam', role: 'View', users: true, workers: false });
      expect(service.updateProjectGrant).not.toHaveBeenCalled();
    });

    it('adds users to a team that only grants workers', () => {
      const { component, service } = setup('project', 'users');
      component.form = { team: 'ops', role: 'Write' };
      component.grant();
      expect(service.updateProjectGrant).toHaveBeenCalledWith('acme', 'ops', { users: true, role: 'Write' });
      expect(service.grantProject).not.toHaveBeenCalled();
    });

    it('keeps the workers of a grant when removing its users', () => {
      const { component, service, changed } = setup('project', 'users');
      component.remove(grantOf('platform'));
      expect(service.updateProjectGrant).toHaveBeenCalledWith('acme', 'platform', { users: false });
      expect(service.removeProjectGrant).not.toHaveBeenCalled();
      expect(changed).toHaveBeenCalled();
    });

    it('deletes a grant that only has users', () => {
      const { component, service } = setup('project', 'users');
      component.remove(grantOf('docs'));
      expect(service.removeProjectGrant).toHaveBeenCalledWith('acme', 'docs');
      expect(service.updateProjectGrant).not.toHaveBeenCalled();
    });
  });

  describe('for workers', () => {
    it('lists only the grants that include workers and marks pending ones', () => {
      const { text } = setup('project', 'workers');
      expect(text()).toContain('Platform');
      expect(text()).toContain('Ops');
      expect(text()).toContain('Pending');
      expect(text()).not.toContain('Docs');
      expect(text()).not.toContain('Role');
    });

    it('creates a workers-only grant for a team without one', () => {
      const { component, service } = setup('project', 'workers');
      component.form = { team: 'newteam', role: 'Admin' };
      component.grant();
      expect(service.grantProject).toHaveBeenCalledWith('acme', { team: 'newteam', users: false, workers: true });
    });

    it('adds workers to a team that only grants users', () => {
      const { component, service } = setup('project', 'workers');
      component.form = { team: 'docs', role: 'Admin' };
      component.grant();
      expect(service.updateProjectGrant).toHaveBeenCalledWith('acme', 'docs', { workers: true });
      expect(service.grantProject).not.toHaveBeenCalled();
    });

    it('keeps the users of a grant when removing its workers', () => {
      const { component, service } = setup('project', 'workers');
      component.remove(grantOf('platform'));
      expect(service.updateProjectGrant).toHaveBeenCalledWith('acme', 'platform', { workers: false });
      expect(service.removeProjectGrant).not.toHaveBeenCalled();
    });

    it('deletes a grant that only has workers', () => {
      const { component, service } = setup('project', 'workers');
      component.remove(grantOf('ops'));
      expect(service.removeProjectGrant).toHaveBeenCalledWith('acme', 'ops');
    });
  });

  it('shows a failed grant inside the grant dialog', () => {
    const { fixture, component, service, text } = setup('project', 'users');
    service.grantProject.mockReturnValue(throwError(() => new Error('Team not found')));
    component.openGrant();
    fixture.detectChanges();
    component.form = { team: 'unknown', role: 'View' };
    component.grant();
    fixture.detectChanges();
    expect(document.querySelector('.gr-dialog')?.textContent).toContain('Team not found');
    expect(text()).not.toContain('Team not found');
  });

  it('grants a cache only users with a role', () => {
    const { component, service } = setup('cache', 'workers');
    component.form = { team: 'platform', role: 'View' };
    component.grant();
    expect(service.grantCache).toHaveBeenCalledWith('acme', 'platform', 'View');
  });
});
