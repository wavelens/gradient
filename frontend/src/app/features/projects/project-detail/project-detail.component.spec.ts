/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { EVERY_PERMISSION } from '@core/resolvers/access-route.testing';
import type { ProjectResponse } from '@core/models';
import { TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { Observable, Subject, of, throwError } from 'rxjs';
import { ProjectDetailComponent } from './project-detail.component';
import { AuthService } from '@core/services/auth.service';
import { StarsService } from '@core/services/stars.service';
import { TasksService } from '@core/services/tasks.service';
import { ApiError } from '@core/services/api.service';

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

describe('ProjectDetailComponent create task', () => {
  it('keeps a failed create inside the open Create Task dialog', () => {
    const { fixture } = render(true);
    const service = TestBed.inject(TasksService) as unknown as { createTask: () => unknown };
    service.createTask = () => throwError(() => new ApiError('Repository is not reachable', 400));
    const cmp = fixture.componentInstance;
    cmp.showCreateDialog.set(true);
    cmp.newTask = {
      name: 'web',
      display_name: 'Web',
      description: '',
      repository: 'https://github.com/acme/web',
      wildcard: 'packages.x86_64-linux.**',
    };
    cmp.createTask();
    fixture.detectChanges();
    expect(document.querySelector('.gr-dialog gr-message-banner')?.textContent).toContain('Repository is not reachable');
    expect(cmp.showCreateDialog()).toBe(true);
  });

  it('sends one create and stays open while the request runs', () => {
    const { fixture } = render(true);
    const createTask = vi.fn(() => new Subject<void>());
    (TestBed.inject(TasksService) as unknown as { createTask: typeof createTask }).createTask = createTask;
    const cmp = fixture.componentInstance;
    cmp.showCreateDialog.set(true);
    cmp.newTask = {
      name: 'web',
      display_name: 'Web',
      description: '',
      repository: 'https://github.com/acme/web',
      wildcard: 'packages.x86_64-linux.**',
    };
    cmp.createTask();
    fixture.detectChanges();
    cmp.createTask();
    const dialog = document.querySelector('.gr-dialog')!;
    dialog.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', keyCode: 27, bubbles: true }));
    fixture.detectChanges();
    expect(createTask).toHaveBeenCalledTimes(1);
    expect(cmp.showCreateDialog()).toBe(true);
    expect(dialog.getAttribute('aria-busy')).toBe('true');
  });

  it('says what is missing when Create is clicked on an empty form', () => {
    const { fixture } = render(true);
    const createTask = vi.fn(() => of(undefined));
    (TestBed.inject(TasksService) as unknown as { createTask: typeof createTask }).createTask = createTask;
    const cmp = fixture.componentInstance;
    cmp.openCreateDialog();
    fixture.detectChanges();
    const dialog = document.querySelector('.gr-dialog')!;
    const create = Array.from(dialog.querySelectorAll('button')).find((b) => b.textContent?.trim() === 'Create')!;
    expect(create.disabled).toBe(false);
    create.click();
    fixture.detectChanges();
    expect(dialog.querySelector('gr-message-banner')?.textContent).toContain('Enter a display name and a name.');
    expect(dialog.querySelector('.field-error')?.textContent).toContain('Enter the repository URL.');
    expect(createTask).not.toHaveBeenCalled();
  });

  it('refuses a local repository path with a reason and sends nothing', () => {
    const { fixture } = render(true);
    const createTask = vi.fn(() => of(undefined));
    (TestBed.inject(TasksService) as unknown as { createTask: typeof createTask }).createTask = createTask;
    const cmp = fixture.componentInstance;
    cmp.openCreateDialog();
    cmp.newTask = { ...cmp.newTask, name: 'web', display_name: 'Web', repository: 'file:///etc' };
    cmp.createTask();
    fixture.detectChanges();
    expect(document.querySelector('.gr-dialog .field-error')?.textContent).toContain('remote repository');
    expect(createTask).not.toHaveBeenCalled();
  });
});
