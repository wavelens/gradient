/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { EMPTY, Observable, Subject, of, throwError } from 'rxjs';
import { ConfirmRequest, ConfirmService } from '@shared/confirm';
import { GrantedPart, TeamGrantsComponent } from './team-grants.component';
import { TeamsService } from '@core/services/teams.service';
import { AuthService } from '@core/services/auth.service';
import { NotifyService } from '@core/services/notify.service';
import { ApiError } from '@core/services/api.service';
import { TeamGrantEntry, TeamSummary } from '@core/models';
import { By } from '@angular/platform-browser';
import { CreateTeamDialogComponent } from '@shared/forms';

const grants: TeamGrantEntry[] = [
  { team: 'platform', display_name: 'Platform', role: 'Write', users: true, workers: true, pending: false },
  { team: 'docs', display_name: 'Docs', role: 'View', users: true, workers: false, pending: false },
  { team: 'ops', display_name: 'Ops', role: null, users: false, workers: true, pending: false },
  { team: 'qa', display_name: 'QA', role: null, users: false, workers: true, pending: true },
];

const myTeams: TeamSummary[] = [
  { name: 'platform', display_name: 'Platform', role: 'member' },
  { name: 'infra', display_name: 'Build Farm', role: 'admin' },
];

function setup(kind: 'project' | 'cache', part?: GrantedPart, superuser = false) {
  const service = {
    list: () => of(myTeams),
    projectGrants: () => of(grants),
    cacheGrants: () => of(grants.filter((g) => g.users)),
    grantProject: vi.fn().mockReturnValue(of('Team granted')),
    grantCache: vi.fn().mockReturnValue(of('Team granted')),
    removeProjectGrant: vi.fn().mockReturnValue(of('ok')),
    removeCacheGrant: vi.fn().mockReturnValue(of('ok')),
    updateProjectGrant: vi.fn().mockReturnValue(of('ok')),
    updateCacheGrant: vi.fn().mockReturnValue(of('ok')),
  };
  const notify = { success: vi.fn(), failure: vi.fn() };
  const confirm = vi.fn((request: ConfirmRequest): Observable<unknown> => request.action());
  TestBed.configureTestingModule({
    imports: [TeamGrantsComponent],
    providers: [
      provideRouter([]),
      { provide: TeamsService, useValue: service },
      { provide: AuthService, useValue: { isAuthenticated: () => true, user: signal({ superuser }) } },
      { provide: NotifyService, useValue: notify },
      { provide: ConfirmService, useValue: { confirm } },
    ],
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
  return { fixture, component: fixture.componentInstance, service, text, changed, notify, confirm };
}

function grantOf(team: string): TeamGrantEntry {
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

  it('links only the teams the viewer can open', () => {
    const { fixture } = setup('project', 'users');
    const row = (name: string) =>
      Array.from((fixture.nativeElement as HTMLElement).querySelectorAll('gr-row')).find((r) => r.textContent?.includes(name))!;
    expect(row('Platform').querySelector('a.row-link')).not.toBeNull();
    expect(row('Docs').querySelector('a.row-link')).toBeNull();
  });

  it('links every team for a superuser', () => {
    const { fixture } = setup('project', 'users', true);
    expect((fixture.nativeElement as HTMLElement).querySelectorAll('a.row-link').length).toBe(2);
  });

  it('suggests teams by their display name', () => {
    const { component } = setup('project', 'users');
    component.search({ query: 'farm' });
    expect(component.suggestions()).toEqual(['infra']);
  });

  it('grants the team whose display name was typed', () => {
    const { component, service } = setup('project', 'users');
    component.form = { team: 'build farm', role: 'View' };
    component.grant();
    expect(service.grantProject).toHaveBeenCalledWith('acme', { team: 'infra', role: 'View', users: true, workers: false });
  });

  it.each(['project', 'cache'] as const)('asks for a team on the %s grant dialog instead of ignoring the click', (kind) => {
    const { fixture, component, service } = setup(kind, kind === 'project' ? 'users' : undefined);
    component.openGrant();
    fixture.detectChanges();
    const dialog = document.querySelector('.gr-dialog')!;
    Array.from(dialog.querySelectorAll<HTMLButtonElement>('button'))
      .find((button) => button.textContent?.trim() === 'Grant')!
      .click();
    fixture.detectChanges();
    expect(dialog.textContent).toContain('Enter a team.');
    expect(service.grantProject).not.toHaveBeenCalled();
    expect(service.grantCache).not.toHaveBeenCalled();
  });

  it('shows a failed grant inside the grant dialog', () => {
    const { fixture, component, service, text } = setup('project', 'users');
    service.grantProject.mockReturnValue(throwError(() => new ApiError('Team not found', 404)));
    component.openGrant();
    fixture.detectChanges();
    component.form = { team: 'unknown', role: 'View' };
    component.grant();
    fixture.detectChanges();
    expect(document.querySelector('.gr-dialog')?.textContent).toContain('Team not found');
    expect(text()).not.toContain('Team not found');
  });

  it('keeps the grant dialog open and sends one request while the grant runs', () => {
    const { fixture, component, service } = setup('project', 'users');
    service.grantProject.mockReturnValue(new Subject());
    component.openGrant();
    fixture.detectChanges();
    component.form = { team: 'infra', role: 'View' };
    const dialog = document.querySelector('.gr-dialog')!;
    const grantButton = Array.from(dialog.querySelectorAll<HTMLButtonElement>('button')).find(
      (button) => button.textContent?.trim() === 'Grant',
    )!;
    grantButton.click();
    fixture.detectChanges();
    grantButton.click();
    dialog.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', keyCode: 27, bubbles: true }));
    (document.querySelector('.gr-dialog-backdrop') as HTMLElement).click();
    fixture.detectChanges();
    expect(service.grantProject).toHaveBeenCalledTimes(1);
    expect(component.showGrant()).toBe(true);
  });

  describe('removal', () => {
    it('asks before removing, naming the team, then removes it', () => {
      const { fixture, service, notify, changed, confirm } = setup('cache');
      (fixture.nativeElement as HTMLElement).querySelector<HTMLButtonElement>('gr-row button.gr-button--danger')!.click();
      expect(confirm).toHaveBeenCalledWith(
        expect.objectContaining({ target: 'Platform', consequence: 'Members of Platform lose their role on this cache.' }),
      );
      expect(service.removeCacheGrant).toHaveBeenCalledWith('acme', 'platform');
      expect(notify.success).toHaveBeenCalledWith('Removed team');
      expect(changed).toHaveBeenCalled();
    });

    it('sends nothing when the removal is cancelled', () => {
      const { component, service, changed, confirm } = setup('cache');
      confirm.mockImplementation(() => EMPTY);
      component.remove(grantOf('platform'));
      expect(service.removeCacheGrant).not.toHaveBeenCalled();
      expect(changed).not.toHaveBeenCalled();
    });

    it('says what the project loses when only the workers go', () => {
      const { component, confirm } = setup('project', 'workers');
      component.remove(grantOf('platform'));
      expect(confirm).toHaveBeenCalledWith(
        expect.objectContaining({ consequence: 'This project can no longer run builds on Platform workers.' }),
      );
    });

    it('warns that removing one part of a pending grant withdraws the whole request', () => {
      const pendingBoth: TeamGrantEntry = { team: 'lab', display_name: 'Lab', role: 'View', users: true, workers: true, pending: true };
      const { component, service, confirm } = setup('project', 'users');
      component.remove(pendingBoth);
      expect(confirm).toHaveBeenCalledWith(
        expect.objectContaining({
          consequence: 'The whole pending request for Lab can no longer get approval, with workers included.',
        }),
      );
      expect(service.removeProjectGrant).toHaveBeenCalledWith('acme', 'lab');
    });
  });

  it('grants a cache only users with a role', () => {
    const { component, service } = setup('cache', 'workers');
    component.form = { team: 'platform', role: 'View' };
    component.grant();
    expect(service.grantCache).toHaveBeenCalledWith('acme', 'platform', 'View');
  });

  it('creates a missing team from the grant dialog and returns with it selected', () => {
    const { fixture, component } = setup('project', 'users');
    component.openGrant();
    fixture.detectChanges();
    component.createInsteadOfPick();
    fixture.detectChanges();
    expect(component.showGrant()).toBe(false);
    expect(component.showCreateTeam()).toBe(true);
    fixture.debugElement.query(By.directive(CreateTeamDialogComponent)).componentInstance.created.emit('newteam');
    expect(component.showGrant()).toBe(true);
    expect(component.form.team).toBe('newteam');
  });
});
