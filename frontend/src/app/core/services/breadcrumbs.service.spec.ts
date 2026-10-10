/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { Subject, of, throwError } from 'rxjs';
import { BreadcrumbsService } from './breadcrumbs.service';
import { CachesService } from './caches.service';
import { ProjectsService } from './projects.service';
import { TasksService } from './tasks.service';
import { TeamsService } from './teams.service';

function setup(overrides: { getProject?: () => unknown } = {}) {
  const getProject = vi.fn(overrides.getProject ?? (() => of({ display_name: 'MyProject' })));
  const getTaskInfo = vi.fn(() => of({ display_name: 'MyTask' }));
  TestBed.configureTestingModule({
    providers: [
      { provide: ProjectsService, useValue: { getProject } },
      { provide: TasksService, useValue: { getTaskInfo } },
      { provide: CachesService, useValue: { getCache: () => of({ display_name: 'MyCache' }) } },
      { provide: TeamsService, useValue: { get: () => of({ display_name: 'MyTeam' }) } },
    ],
  });

  return { crumbs: TestBed.inject(BreadcrumbsService), getProject, getTaskInfo };
}

const labels = (trail: { label: string | null }[]) => trail.map((crumb) => crumb.label);

describe('BreadcrumbsService', () => {
  it('leads a task settings subpage through every level by display name', () => {
    const { crumbs } = setup();

    const trail = crumbs.taskSettings('acme', 'build', { label: 'Triggers' });

    expect(labels(trail)).toEqual(['Projects', 'MyProject', 'MyTask', 'Settings', 'Triggers']);
    expect(trail.map((crumb) => crumb.link)).toEqual([
      ['/projects'],
      ['/project', 'acme'],
      ['/project', 'acme', 'task', 'build'],
      ['/project', 'acme', 'task', 'build', 'settings'],
      undefined,
    ]);
  });

  it('starts cache and team trails at the list pages', () => {
    const { crumbs } = setup();

    expect(labels(crumbs.cacheSettings('main', { label: 'NARs' }))).toEqual(['Caches', 'MyCache', 'Settings', 'NARs']);
    expect(labels(crumbs.team('core', { label: 'Members' }))).toEqual(['Teams', 'MyTeam', 'Members']);
  });

  it('holds a name back until it is known instead of showing the URL name', () => {
    const project = new Subject<{ display_name: string }>();
    const { crumbs } = setup({ getProject: () => project });

    expect(labels(crumbs.project('acme'))).toEqual(['Projects', null]);
    project.next({ display_name: 'MyProject' });
    expect(labels(crumbs.project('acme'))).toEqual(['Projects', 'MyProject']);
  });

  it('falls back to the URL name when the lookup fails', () => {
    const { crumbs } = setup({ getProject: () => throwError(() => new Error('forbidden')) });

    expect(labels(crumbs.project('acme'))).toEqual(['Projects', 'acme']);
  });

  it('looks a name up a single time for every page that shows it', () => {
    const { crumbs, getProject } = setup();

    crumbs.project('acme');
    crumbs.projectSettings('acme', { label: 'Workers' });
    crumbs.task('acme', 'build');

    expect(getProject).toHaveBeenCalledTimes(1);
  });

  it('takes a name a page already holds without a lookup', () => {
    const { crumbs, getProject, getTaskInfo } = setup();

    crumbs.rememberProject('acme', 'Renamed');
    crumbs.rememberTask('acme', 'build', 'Nightly');

    expect(labels(crumbs.task('acme', 'build'))).toEqual(['Projects', 'Renamed', 'Nightly']);
    expect(getProject).not.toHaveBeenCalled();
    expect(getTaskInfo).not.toHaveBeenCalled();
  });
});
