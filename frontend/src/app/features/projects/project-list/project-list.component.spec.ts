/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { Router, provideRouter } from '@angular/router';
import { Observable, Subject, of, throwError } from 'rxjs';
import { ProjectListComponent } from './project-list.component';
import { AuthService } from '@core/services/auth.service';
import { ConfigService } from '@core/services/config.service';
import { ProjectsService } from '@core/services/projects.service';
import { ApiError } from '@core/services/api.service';

const EMPTY_PAGE = { items: [], total: 0, page: 1 };

function render(getProjects: () => Observable<unknown>, getPublicProjects: () => Observable<unknown> = () => of(EMPTY_PAGE), signedIn = true) {
  const projects = vi.fn(getProjects);
  TestBed.configureTestingModule({
    imports: [ProjectListComponent],
    providers: [
      provideRouter([]),
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

  it('drops the own empty list for a signed-in non-member while public projects are listed', () => {
    const publicPage = { items: [{ id: 'p1', name: 'nixpkgs', display_name: 'Nixpkgs', description: null }], total: 1, page: 1 };
    const { root } = render(() => of(EMPTY_PAGE), () => of(publicPage));
    expect(root.textContent).not.toContain('No projects yet');
    expect(root.textContent).toContain('Nixpkgs');
    expect(Array.from(root.querySelectorAll('button')).filter((b) => b.textContent?.includes('Create Project'))).toHaveLength(1);
  });

  it('tells anonymous visitors when nothing is public', () => {
    const { root } = render(() => of(EMPTY_PAGE), () => of(EMPTY_PAGE), false);
    expect(root.textContent).toContain('No public projects');
    expect(root.textContent).not.toContain('No projects yet');
  });

  it('keeps a failed create inside the open Create Project dialog', () => {
    const { fixture } = render(() => of(EMPTY_PAGE));
    const service = TestBed.inject(ProjectsService) as unknown as { createProject: () => unknown };
    service.createProject = () => throwError(() => new ApiError('Project name acme is taken', 409));
    const cmp = fixture.componentInstance;
    cmp.openCreateDialog();
    cmp.newProject.name = 'acme';
    cmp.newProject.display_name = 'Acme';
    cmp.createProject();
    fixture.detectChanges();
    expect(document.querySelector('.gr-dialog gr-message-banner')?.textContent).toContain('Project name acme is taken');
    expect(cmp.showCreateDialog()).toBe(true);
  });

  it('opens the created project', () => {
    const { fixture } = render(() => of(EMPTY_PAGE));
    (TestBed.inject(ProjectsService) as unknown as { createProject: () => unknown }).createProject = () => of('Project created');
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigateByUrl').mockResolvedValue(true);
    const cmp = fixture.componentInstance;
    cmp.openCreateDialog();
    cmp.newProject.name = 'acme';
    cmp.newProject.display_name = 'Acme';
    cmp.createProject();
    expect(String(navigate.mock.calls[0][0])).toBe('/project/acme');
  });

  it('ignores Enter while the project name is still being checked', () => {
    const { fixture } = render(() => of(EMPTY_PAGE));
    const createProject = vi.fn(() => of(undefined));
    (TestBed.inject(ProjectsService) as unknown as { createProject: typeof createProject }).createProject = createProject;
    const cmp = fixture.componentInstance;
    cmp.openCreateDialog();
    cmp.newProject.display_name = 'Acme';
    cmp.newProject.name = 'acme';
    (cmp as unknown as { names: { check(name: string): void } }).names.check('acme');
    cmp.createProject();
    expect(createProject).not.toHaveBeenCalled();
  });

  it('sends one create and stays open while the request runs', () => {
    const { fixture } = render(() => of(EMPTY_PAGE));
    const createProject = vi.fn(() => new Subject<void>());
    (TestBed.inject(ProjectsService) as unknown as { createProject: typeof createProject }).createProject = createProject;
    const cmp = fixture.componentInstance;
    cmp.openCreateDialog();
    cmp.newProject.name = 'acme';
    cmp.newProject.display_name = 'Acme';
    cmp.createProject();
    fixture.detectChanges();
    cmp.createProject();
    const dialog = document.querySelector('.gr-dialog')!;
    dialog.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', keyCode: 27, bubbles: true }));
    fixture.detectChanges();
    expect(createProject).toHaveBeenCalledTimes(1);
    expect(cmp.showCreateDialog()).toBe(true);
    expect(dialog.getAttribute('aria-busy')).toBe('true');
  });

  it('says what is missing when Create is clicked on an empty form', () => {
    const { fixture } = render(() => of(EMPTY_PAGE));
    const createProject = vi.fn(() => of(undefined));
    (TestBed.inject(ProjectsService) as unknown as { createProject: typeof createProject }).createProject = createProject;
    fixture.componentInstance.openCreateDialog();
    fixture.detectChanges();
    const dialog = document.querySelector('.gr-dialog')!;
    Array.from(dialog.querySelectorAll('button')).find((b) => b.textContent?.trim() === 'Create')!.click();
    fixture.detectChanges();
    expect(dialog.querySelector('gr-message-banner')?.textContent).toContain('Enter a display name and a name.');
    expect(createProject).not.toHaveBeenCalled();
  });
});
