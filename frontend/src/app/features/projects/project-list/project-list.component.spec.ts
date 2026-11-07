/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { Observable, of, throwError } from 'rxjs';
import { ProjectListComponent } from './project-list.component';
import { AuthService } from '@core/services/auth.service';
import { ConfigService } from '@core/services/config.service';
import { ProjectsService } from '@core/services/projects.service';
import { ApiError } from '@core/services/api.service';

const EMPTY_PAGE = { items: [], total: 0, page: 1 };

function render(getProjects: () => Observable<unknown>, getPublicProjects: () => Observable<unknown> = () => of(EMPTY_PAGE)) {
  const projects = vi.fn(getProjects);
  TestBed.configureTestingModule({
    imports: [ProjectListComponent],
    providers: [
      provideRouter([]),
      { provide: AuthService, useValue: { isAuthenticated: () => true, user: () => ({ superuser: false }) } },
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
});
