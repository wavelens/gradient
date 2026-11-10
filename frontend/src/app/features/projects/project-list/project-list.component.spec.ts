/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { PendingInvitesService } from '@core/services/pending-invites.service';
import { Router, provideRouter } from '@angular/router';
import { By } from '@angular/platform-browser';
import { CreateProjectDialogComponent } from '@shared/forms';
import { Observable, of, throwError } from 'rxjs';
import { ProjectListComponent } from './project-list.component';
import { AuthService } from '@core/services/auth.service';
import { ConfigService } from '@core/services/config.service';
import { ProjectsService } from '@core/services/projects.service';
import { TeamsService } from '@core/services/teams.service';

const EMPTY_PAGE = { items: [], total: 0, page: 1 };

function render(
  getProjects: () => Observable<unknown>,
  getPublicProjects: () => Observable<unknown> = () => of(EMPTY_PAGE),
  signedIn = true,
  invitationCount = 0,
) {
  const projects = vi.fn(getProjects);
  TestBed.configureTestingModule({
    imports: [ProjectListComponent],
    providers: [
      provideRouter([]),
      { provide: TeamsService, useValue: { list: () => of([]) } },
      { provide: PendingInvitesService, useValue: { count: signal(invitationCount), all: signal([...Array(invitationCount).fill({ kind: 'project' }), { kind: 'cache' }]) } },
      { provide: AuthService, useValue: { isAuthenticated: () => signedIn, user: () => (signedIn ? { superuser: false } : null) } },
      { provide: ConfigService, useValue: { publicListing: true, createProject: 'everyone', canCreate: () => true } },
      {
        provide: ProjectsService,
        useValue: { getProjects: projects, getPublicProjects, checkProjectNameAvailable: () => of(true) },
      },
    ],
  });
  const fixture = TestBed.createComponent(ProjectListComponent);
  fixture.detectChanges();
  return { root: fixture.nativeElement as HTMLElement, projects, fixture };
}

describe('ProjectListComponent', () => {
  it('offers to create the first project when the list is empty', () => {
    expect(render(() => of(EMPTY_PAGE)).root.textContent).toContain('No projects yet');
  });

  it('mark only projects with a role below Admin', () => {
    const page = {
      items: [
        { id: 'p1', name: 'own', display_name: 'Own', role: 'Admin' },
        { id: 'p2', name: 'shared', display_name: 'Shared', role: 'View' },
      ],
      total: 2,
      page: 1,
    };
    const { root } = render(() => of(page));
    expect(Array.from(root.querySelectorAll('gr-badge')).map((b) => b.textContent?.trim())).toEqual(['View']);
  });

  it('shows building and waiting evaluations as separate chips', () => {
    const page = {
      items: [
        { id: 'p1', name: 'busy', display_name: 'Busy', role: 'Admin', running_evaluations: 2, waiting_evaluations: 1 },
        { id: 'p2', name: 'idle', display_name: 'Idle', role: 'Admin', running_evaluations: 0, waiting_evaluations: 0 },
      ],
      total: 2,
      page: 1,
    };
    const { root } = render(() => of(page));
    expect(Array.from(root.querySelectorAll('gr-badge')).map((b) => b.textContent?.trim())).toEqual(['2 building', '1 waiting']);
  });

  it('reloads the list while the tab stays open', () => {
    vi.useFakeTimers();
    try {
      const { projects } = render(() => of(EMPTY_PAGE));
      expect(projects).toHaveBeenCalledTimes(1);
      vi.advanceTimersByTime(15_000);
      expect(projects).toHaveBeenCalledTimes(2);
    } finally {
      vi.useRealTimers();
    }
  });

  it('shows a failed load as an error, never as "No projects yet"', () => {
    const { root } = render(() => throwError(() => ({ status: 500, message: 'database is down' })));
    expect(root.textContent).toContain('Failed to load projects');
    expect(root.textContent).toContain('database is down');
    expect(root.textContent).not.toContain('No projects yet');
  });

  it('loads the projects again on Try Again', () => {
    const { root, projects, fixture } = render(() => throwError(() => new Error('down')));
    projects.mockImplementation(() => of(EMPTY_PAGE));
    root.querySelector<HTMLButtonElement>('.gr-load-state__error button')!.click();
    fixture.detectChanges();
    expect(projects).toHaveBeenCalledTimes(2);
    expect(root.textContent).toContain('No projects yet');
  });

  it('reports a failed public list next to the own projects', () => {
    const { root } = render(() => of(EMPTY_PAGE), () => throwError(() => new Error('down')));
    expect(root.textContent).toContain('Failed to load public projects');
  });

  it('shows anonymous visitors the public projects without an own empty list', () => {
    const publicPage = { items: [{ id: 'p1', name: 'nixpkgs', display_name: 'Nixpkgs', description: null }], total: 1, page: 1 };
    const { root, projects } = render(() => of(EMPTY_PAGE), () => of(publicPage), false);
    expect(projects).not.toHaveBeenCalled();
    expect(root.textContent).not.toContain('No projects yet');
    expect(root.textContent).toContain('Nixpkgs');
    expect(root.textContent).toContain('Sign in');
  });

  it('leaves the Public badge off the cards under Discover Public Projects, whose heading already says so', () => {
    const publicPage = { items: [{ id: 'p1', name: 'nixpkgs', display_name: 'Nixpkgs', description: null }], total: 1, page: 1 };
    const { root } = render(() => of(EMPTY_PAGE), () => of(publicPage));
    expect(root.textContent).toContain('Discover Public Projects');
    expect(root.querySelectorAll('gr-badge').length).toBe(0);
  });

  it('keeps the own empty list for a signed-in non-member next to the public projects', () => {
    const publicPage = { items: [{ id: 'p1', name: 'nixpkgs', display_name: 'Nixpkgs', description: null }], total: 1, page: 1 };
    const { root } = render(() => of(EMPTY_PAGE), () => of(publicPage));
    expect(root.textContent).toContain('No projects yet');
    expect(root.textContent).toContain('Nixpkgs');
    expect(Array.from(root.querySelectorAll('button')).filter((b) => b.textContent?.includes('Create Project'))).toHaveLength(1);
  });

  it('points a signed-in user with invitations to them above the list', () => {
    const { root } = render(() => of(EMPTY_PAGE), undefined, true, 2);
    expect(root.querySelector('app-pending-invitations-banner')?.textContent).toContain('2 pending project invitations');
  });

  it('tells anonymous visitors when nothing is public', () => {
    const { root } = render(() => of(EMPTY_PAGE), () => of(EMPTY_PAGE), false);
    expect(root.textContent).toContain('No public projects');
    expect(root.textContent).not.toContain('No projects yet');
  });

  it('names the owner team next to the project name', () => {
    const page = { items: [{ id: 'p1', name: 'acme', display_name: 'Acme', role: 'Admin', owner_team: { name: 'infra', display_name: 'Infra' } }], total: 1, page: 1 };
    const { root } = render(() => of(page));
    expect(root.querySelector('gr-nav-card gr-entity-name')?.getAttribute('title')).toBe('Acme (acme) · Infra');
  });

  it('opens the created project', () => {
    const { fixture } = render(() => of(EMPTY_PAGE));
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigateByUrl').mockResolvedValue(true);
    fixture.debugElement.query(By.directive(CreateProjectDialogComponent)).componentInstance.created.emit('acme');
    expect(String(navigate.mock.calls[0][0])).toBe('/project/acme');
  });
});
