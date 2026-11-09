/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { EVERY_PERMISSION } from '@core/resolvers/access-route.testing';
import type { CacheSubscriptionItem, ProjectResponse } from '@core/models';
import { ProjectsService } from '@core/services/projects.service';
import { TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { NEVER, Observable, of, throwError } from 'rxjs';
import { ProjectDetailComponent } from './project-detail.component';
import { AuthService } from '@core/services/auth.service';
import { StarsService } from '@core/services/stars.service';
import { TasksService } from '@core/services/tasks.service';

const NO_TASKS = { items: [], total: 0, page: 1 };

function render(
  authenticated: boolean,
  getTasks: () => Observable<unknown> = () => of(NO_TASKS),
  access: Partial<ProjectResponse> = { role: 'Admin', permissions: EVERY_PERMISSION },
  subscribedCaches: CacheSubscriptionItem[] = [],
) {
  const starred = vi.fn(() => of(true));
  const tasks = vi.fn(getTasks);
  const getSubscribedCaches = vi.fn(() => of(subscribedCaches));
  TestBed.configureTestingModule({
    imports: [ProjectDetailComponent],
    providers: [
      provideRouter([]),
      {
        provide: ActivatedRoute,
        useValue: {
          snapshot: {
            paramMap: convertToParamMap({ project: 'acme' }),
            data: { projectAccess: { project: { name: 'acme', display_name: 'Acme', description: '', ...access } } },
          },
        },
      },
      { provide: AuthService, useValue: { isAuthenticated: () => authenticated } },
      { provide: TasksService, useValue: { getTasks: tasks } },
      { provide: StarsService, useValue: { starred, set: () => of(true) } },
      { provide: ProjectsService, useValue: { getSubscribedCaches, getProject: () => NEVER } },
    ],
  });
  const fixture = TestBed.createComponent(ProjectDetailComponent);
  fixture.detectChanges();
  return { root: fixture.nativeElement as HTMLElement, starred, tasks, fixture, getSubscribedCaches };
}

describe('ProjectDetailComponent subscribed caches', () => {
  const caches: CacheSubscriptionItem[] = [
    { id: 'c1', name: 'main', display_name: 'Main', mode: 'ReadWrite', status: 'active' },
    { id: 'c2', name: 'research', display_name: 'Research', mode: 'ReadOnly', status: 'active' },
    { id: 'c3', name: 'waiting', display_name: 'Waiting', mode: 'ReadOnly', status: 'pending' },
  ];

  it('names the active caches in the header and links to the cache subscriptions', () => {
    const { root } = render(true, undefined, undefined, caches);
    const line = root.querySelector<HTMLAnchorElement>('[slot="meta"]')!;
    expect(line.textContent?.trim()).toBe('Caches: main, research');
    expect(line.getAttribute('href')).toBe('/project/acme/caches');
  });

  it('asks nothing for a visitor without a role and shows no cache line', () => {
    const { root, getSubscribedCaches } = render(false, undefined, { role: null, permissions: [] }, caches);
    expect(getSubscribedCaches).not.toHaveBeenCalled();
    expect(root.querySelector('[slot="meta"]')).toBeNull();
  });
});

describe('ProjectDetailComponent header', () => {
  it('stars the project from its header', () => {
    const { root, starred } = render(true);
    expect(starred).toHaveBeenCalledWith({ kind: 'project', project: 'acme' });
    expect(root.querySelector('gr-star-button button')!.getAttribute('aria-pressed')).toBe('true');
    expect(root.querySelector('gr-star-button button')!.textContent!.trim()).toBe('Starred');
  });

  it('shows no star to a guest', () => {
    expect(render(false).root.querySelector('gr-star-button')).toBeNull();
  });

  it('offers a viewer neither Settings nor Connect repository, like the task header', () => {
    const { root } = render(true, undefined, { role: 'View', permissions: ['viewProject'] });
    const labels = Array.from(root.querySelectorAll('[slot="actions"] [grButton]')).map((el) => el.querySelector('.gr-button__label')?.textContent?.trim());
    expect(labels).not.toContain('Settings');
    expect(labels).not.toContain('Connect repository');
    expect(root.textContent).toContain('Ask a project administrator to connect a repository.');
  });

  it('offers neither Settings nor Connect repository to a visitor without a role', () => {
    const { root } = render(true, undefined, { role: null, permissions: [] });
    const labels = Array.from(root.querySelectorAll('[slot="actions"] [grButton]')).map((el) => el.querySelector('.gr-button__label')?.textContent?.trim());
    expect(labels).not.toContain('Settings');
    expect(labels).not.toContain('Connect repository');
  });
});

describe('ProjectDetailComponent tasks', () => {
  it('shows the empty state when the project has no tasks', () => {
    expect(render(true).root.textContent).toContain('No tasks yet');
  });

  it('shows a failed task load as an error, never as an empty project', () => {
    const { root } = render(true, () => throwError(() => new Error('database is down')));
    expect(root.textContent).toContain('Failed to load tasks');
    expect(root.textContent).toContain('database is down');
    expect(root.textContent).not.toContain('No tasks yet');
  });

  it('loads the tasks again on Try Again', () => {
    const { root, tasks, fixture } = render(true, () => throwError(() => new Error('down')));
    tasks.mockImplementation(() => of(NO_TASKS));
    root.querySelector<HTMLButtonElement>('.gr-load-state__error button')!.click();
    fixture.detectChanges();
    expect(tasks).toHaveBeenCalledTimes(2);
    expect(root.textContent).toContain('No tasks yet');
  });
});

describe('ProjectDetailComponent connect repository', () => {
  it('links writers to the Connect repository flow', async () => {
    const { root, fixture } = render(true);
    await fixture.whenStable();
    fixture.detectChanges();
    const links = Array.from(root.querySelectorAll('a')).filter((a) => a.textContent?.includes('Connect repository'));
    expect(links.map((a) => a.getAttribute('href'))).toContain('/project/acme/connect');
  });

  it('tells view-only members whom to ask instead of offering the flow', async () => {
    const { root, fixture } = render(true, undefined, { role: 'View', permissions: ['viewProject'] });
    await fixture.whenStable();
    fixture.detectChanges();
    expect(root.textContent).not.toContain('Connect repository');
    expect(root.textContent).toContain('Ask a project administrator');
  });
});
