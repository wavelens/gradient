/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { EVERY_PERMISSION } from '@core/resolvers/access-route.testing';
import type { ProjectResponse } from '@core/models';
import { TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { Observable, of, throwError } from 'rxjs';
import { ProjectDetailComponent } from './project-detail.component';
import { AuthService } from '@core/services/auth.service';
import { StarsService } from '@core/services/stars.service';
import { TasksService } from '@core/services/tasks.service';

const NO_TASKS = { items: [], total: 0, page: 1 };

function render(
  authenticated: boolean,
  getTasks: () => Observable<unknown> = () => of(NO_TASKS),
  access: Partial<ProjectResponse> = { role: 'Admin', permissions: EVERY_PERMISSION },
) {
  const starred = vi.fn(() => of(true));
  const tasks = vi.fn(getTasks);
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
    ],
  });
  const fixture = TestBed.createComponent(ProjectDetailComponent);
  fixture.detectChanges();
  return { root: fixture.nativeElement as HTMLElement, starred, tasks, fixture };
}

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

  it('offers a viewer Settings but no Create Task', () => {
    const { root } = render(true, undefined, { role: 'View', permissions: ['viewProject'] });
    const labels = Array.from(root.querySelectorAll('[slot="actions"] [grButton]')).map((el) => el.querySelector('.gr-button__label')?.textContent?.trim());
    expect(labels).toContain('Settings');
    expect(labels).not.toContain('Create Task');
    expect(root.textContent).toContain('No tasks have been created yet');
  });

  it('offers neither Settings nor Create Task to a visitor without a role', () => {
    const { root } = render(true, undefined, { role: null, permissions: [] });
    const labels = Array.from(root.querySelectorAll('[slot="actions"] [grButton]')).map((el) => el.querySelector('.gr-button__label')?.textContent?.trim());
    expect(labels).not.toContain('Settings');
    expect(labels).not.toContain('Create Task');
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
