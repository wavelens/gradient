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
import { EMPTY, Observable, of, throwError } from 'rxjs';
import { ConfirmRequest, ConfirmService } from '@shared/confirm';
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

function setup(role: TeamResponse['role'] = 'member', approveRequest = vi.fn().mockReturnValue(of('ok')), workers: unknown[] = []) {
  const notify = { success: vi.fn(), failure: vi.fn() };
  const confirm = vi.fn((confirmRequest: ConfirmRequest): Observable<unknown> => confirmRequest.action());
  const denyRequest = vi.fn().mockReturnValue(of('ok'));
  TestBed.configureTestingModule({
    imports: [TeamOverviewComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      {
        provide: ActivatedRoute,
        useValue: { snapshot: { paramMap: convertToParamMap({ team: 'platform' }), data: { teamAccess: { team: { ...team, role } } } }, data: of({ teamAccess: { team: { ...team, role } } }) },
      },
      { provide: ConfigService, useValue: { gradientCiEnabled: false } },
      {
        provide: TeamsService,
        useValue: {
          get: () => of({ ...team, role }),
          members: () => of([]),
          invitations: () => of([]),
          workers: () => of(workers),
          grants: () => of({ projects: [], caches: [] }),
          requests: () => of([request]),
          approveRequest,
          denyRequest,
        },
      },
      { provide: AuthService, useValue: { user: signal({ superuser: false }) } },
      { provide: NotifyService, useValue: notify },
      { provide: ConfirmService, useValue: { confirm } },
    ],
  });
  const fixture = TestBed.createComponent(TeamOverviewComponent);
  fixture.detectChanges();
  return { fixture, notify, confirm, denyRequest, root: fixture.nativeElement as HTMLElement };
}

function render(): HTMLElement {
  return setup().root;
}

function headings(element: HTMLElement): string[] {
  return Array.from(element.querySelectorAll('h2'), (heading) => heading.textContent?.trim() ?? '');
}

describe('TeamOverviewComponent', () => {
  it('lists workers below projects and caches and leaves members to the Members page', () => {
    const sections = headings(setup('admin').root).filter((heading) => !heading.startsWith('No '));
    expect(sections).toEqual(['Requests', 'Projects and Caches', 'Workers']);
  });

  it('links every team member to the Members page next to the admin-only Settings', () => {
    const memberHrefs = Array.from(render().querySelectorAll('a[slot="actions"]'), (link) => link.getAttribute('href'));
    expect(memberHrefs).toEqual(['/team/platform/members/people']);
    TestBed.resetTestingModule();
    const adminHrefs = Array.from(setup('admin').root.querySelectorAll('a[slot="actions"]'), (link) => link.getAttribute('href'));
    expect(adminHrefs).toEqual(['/team/platform/members/people', '/team/platform/settings']);
  });

  it('picks up new grants and workers while the page stays open', () => {
    vi.useFakeTimers();
    try {
      const { fixture } = setup();
      const teams = TestBed.inject(TeamsService) as unknown as Record<'grants' | 'workers', () => Observable<unknown>>;
      teams.grants = () => of({ projects: [{ project: 'acme', display_name: 'Acme', users: true, workers: false, role: 'View' }], caches: [] });
      teams.workers = () => of([]);
      vi.advanceTimersByTime(15_000);
      fixture.detectChanges();
      const text = (fixture.nativeElement as HTMLElement).textContent;
      expect(text).toContain('Acme');
      expect(text).toContain('Members get View');
    } finally {
      vi.useRealTimers();
    }
  });

  it('keeps one section title for a new team and gives workers their own empty state', () => {
    const { fixture, root } = setup('admin');
    fixture.detectChanges();
    expect(headings(root)).toContain('Projects and Caches');
    expect(headings(root)).not.toContain('Team Access');
    expect(root.textContent).toContain('Teams tab of the Members page');
    expect(root.textContent).not.toContain('or workers');
    expect(root.textContent).toContain('No online workers');
    const links = Array.from(root.querySelectorAll<HTMLAnchorElement>('a[grButton]')).map((link) => link.getAttribute('href'));
    expect(links).toEqual(expect.arrayContaining(['/projects', '/caches']));
  });

  it('keeps the projects and caches empty state while the team has workers', () => {
    const { fixture, root } = setup('admin', undefined, [{ worker_id: 'w1', display_name: 'Builder', connected: true, active: true }]);
    fixture.detectChanges();
    expect(headings(root)).toContain('Projects and Caches');
    expect(root.textContent).toContain('No projects or caches');
  });

  it('leaves out the empty projects and caches section and the "Team" subtitle for members', () => {
    const root = render();
    expect(headings(root)).not.toContain('Projects and Caches');
    expect(root.querySelector('gr-page-layout')?.textContent).not.toMatch(/^\s*Team\s*$/m);
  });

  it('shows a failed grants request as an error with retry, not as no projects', () => {
    const { fixture, root } = setup();
    const teams = TestBed.inject(TeamsService) as unknown as { grants: () => Observable<unknown> };
    teams.grants = () => throwError(() => new ApiError('Database unavailable', 500));
    fixture.componentInstance.loadGrants();
    fixture.detectChanges();
    expect(root.textContent).toContain('Failed to load projects and caches');
    expect(root.textContent).toContain('Database unavailable');
    expect(root.textContent).not.toContain('No projects or caches');
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

  it('asks before denying a request, naming who asked', () => {
    const { root, notify, confirm, denyRequest } = setup('admin');
    denyButton(root).click();
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ target: 'Acme (acme)', confirmLabel: 'Deny' }));
    expect(denyRequest).toHaveBeenCalledWith('platform', 'r1');
    expect(notify.success).toHaveBeenCalledWith('Request denied');
  });

  it('sends no denial when the confirmation is cancelled', () => {
    const { root, confirm, denyRequest } = setup('admin');
    confirm.mockImplementation(() => EMPTY);
    denyButton(root).click();
    expect(denyRequest).not.toHaveBeenCalled();
  });
});

function denyButton(root: HTMLElement): HTMLButtonElement {
  return Array.from(root.querySelectorAll<HTMLButtonElement>('button')).find(
    (b) => b.querySelector('.gr-button__label')?.textContent?.trim() === 'Deny',
  )!;
}
