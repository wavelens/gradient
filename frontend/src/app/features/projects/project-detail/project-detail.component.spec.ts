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
import { By } from '@angular/platform-browser';
import { StarButtonComponent } from '@shared/ui';

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
    const line = root.querySelector<HTMLAnchorElement>('a[slot="meta"]')!;
    expect(line.textContent?.trim()).toBe('Caches: Main, Research');
    expect(line.getAttribute('href')).toBe('/project/acme/caches');
  });

  it('asks nothing for a visitor without a role and shows no cache line', () => {
    const { root, getSubscribedCaches } = render(false, undefined, { role: null, permissions: [] }, caches);
    expect(getSubscribedCaches).not.toHaveBeenCalled();
    expect(root.querySelector('a[slot="meta"]')).toBeNull();
    expect(root.querySelector('[data-testid="no-cache-banner"]')).toBeNull();
  });

  it('says why evaluations wait only while no active cache accepts writes', () => {
    expect(render(true, undefined, undefined, caches).root.querySelector('[data-testid="no-cache-banner"]')).toBeNull();
    TestBed.resetTestingModule();
    const banner = render(true, undefined, undefined, caches.slice(1)).root.querySelector('[data-testid="no-cache-banner"]');
    expect(banner?.querySelector('a')?.getAttribute('href')).toBe('/project/acme/caches');
    expect(banner?.textContent).toContain('No writable cache');
    expect(banner?.querySelector('a')?.textContent?.trim()).toBe('Add a cache');
  });

  it('names a pending writable subscription request instead of a missing cache', () => {
    const pending: CacheSubscriptionItem = { id: 'c4', name: 'shared', display_name: 'Shared', mode: 'ReadWrite', status: 'pending' };
    const banner = render(true, undefined, undefined, [...caches.slice(1), pending]).root.querySelector('[data-testid="no-cache-banner"]');
    expect(banner?.textContent).toContain('Subscription request pending');
    expect(banner?.textContent).not.toContain('No writable cache');
    expect(banner?.querySelector('a')?.getAttribute('href')).toBe('/project/acme/caches');
  });
});

describe('ProjectDetailComponent task cards', () => {
  it('marks inactive tasks with a neutral badge', () => {
    const task = (name: string, active: boolean) => ({ id: name, name, display_name: name, description: '', active, recent_evaluations: [] });
    const { root } = render(true, () => of({ items: [task('on', true), task('off', false)], total: 2, page: 1 }));
    const metas = Array.from(root.querySelectorAll('gr-nav-card')).map((card) => card.querySelector('[slot="meta"]')?.textContent?.trim());
    expect(metas).toEqual(['', 'Inactive']);
    expect(root.querySelector('gr-badge .badge')?.classList).toContain('is-neutral');
  });

  it('titles each card with the display name and the slug', () => {
    const task = { id: 't1', name: 'build', display_name: 'Nightly Build', description: '', active: true, recent_evaluations: [] };
    const card = render(true, () => of({ items: [task], total: 1, page: 1 })).root.querySelector('gr-nav-card')!;
    expect(card.querySelector('.nav-card__title')?.getAttribute('title')).toBe('Nightly Build (build)');
    expect(card.querySelector('.entity-name__display')?.textContent).toBe('Nightly Build');
    expect(card.querySelector('.entity-name__slug')?.textContent).toBe('build');
  });

  describe('latest evaluations', () => {
    const run = (id: string, status: 'Completed' | 'Failed' | 'Building', finished_at: string | null) =>
      ({ id, status, duration_ms: 60_000, created_at: '2026-11-10 12:00:00', started_at: '2026-11-10 12:05:00', finished_at });
    const cardFor = (recent_evaluations: ReturnType<typeof run>[]) => {
      const task = { id: 't1', name: 'build', display_name: 'Build', description: '', active: true, recent_evaluations };
      return render(false, () => of({ items: [task], total: 1, page: 1 }), { role: null, permissions: [] }).root.querySelector('gr-nav-card')!;
    };

    beforeEach(() => vi.useFakeTimers({ now: new Date('2026-11-10T12:30:00Z'), toFake: ['Date'] }));
    afterEach(() => vi.useRealTimers());

    it('tells a guest when the latest evaluation finished and draws the recent ones without nested links', () => {
      const card = cardFor([run('e1', 'Failed', '2026-11-10 12:10:00'), run('e2', 'Completed', '2026-11-10 12:18:00')]);
      expect(card.querySelector('.task-card-latest-evaluation')?.textContent?.trim()).toBe('Finished 12m ago');
      expect(card.querySelectorAll('app-evaluation-history .bar')).toHaveLength(2);
      expect(card.querySelector('app-evaluation-history a')).toBeNull();
    });

    it('tells how long the latest evaluation runs and draws no history for a single run', () => {
      const card = cardFor([run('e1', 'Building', null)]);
      expect(card.querySelector('.task-card-latest-evaluation')?.textContent?.trim()).toBe('Running for 25m 00s');
      expect(card.querySelector('app-evaluation-history')).toBeNull();
    });
  });
});

describe('ProjectDetailComponent header', () => {
  it('stars the project from its header', () => {
    const { root, starred } = render(true);
    expect(starred).toHaveBeenCalledWith({ kind: 'project', project: 'acme' });
    expect(root.querySelector('gr-star-button button')!.getAttribute('aria-pressed')).toBe('true');
    expect(root.querySelector('gr-star-button button')!.textContent!.trim()).toBe('Starred');
  });

  it('names the project by its display name on the star button', () => {
    const { fixture } = render(true);
    const star = fixture.debugElement.query(By.directive(StarButtonComponent)).componentInstance as StarButtonComponent;
    expect(star.starredItemDisplayName()).toBe('Acme');
  });

  it('shows no star to a guest', () => {
    expect(render(false).root.querySelector('gr-star-button')).toBeNull();
  });

  it('offers the project slug the CLI uses for copying, also to guests', () => {
    const copy = render(false).root.querySelector('gr-copy-field[slot="meta"]');
    expect(copy?.querySelector('code')?.textContent).toBe('acme');
    expect(copy?.querySelector('button')?.getAttribute('aria-label')).toBe('Copy project name for CLI');
  });

  it('offers a viewer neither Settings nor Connect repository, like the task header', () => {
    const { root } = render(true, undefined, { role: 'View', permissions: ['viewProject'] });
    const labels = Array.from(root.querySelectorAll('[slot="actions"] [grButton]')).map((el) => el.querySelector('.gr-button__label')?.textContent?.trim());
    expect(labels).not.toContain('Settings');
    expect(labels).not.toContain('Connect repository');
    expect(root.textContent).toContain('Ask a project administrator to connect a repository.');
    expect(root.textContent?.match(/No tasks yet/g)).toHaveLength(1);
  });

  it('refreshes the header with the tasks on the reload interval', () => {
    vi.useFakeTimers();
    try {
      const { root, fixture } = render(true);
      const projects = TestBed.inject(ProjectsService) as unknown as { getProject: (name: string) => Observable<unknown> };
      const getProject = vi.spyOn(projects, 'getProject').mockReturnValue(
        of({ name: 'acme', display_name: 'Acme Renamed', description: '', role: 'Admin', permissions: EVERY_PERMISSION }),
      );
      vi.advanceTimersByTime(15_000);
      fixture.detectChanges();
      expect(getProject).toHaveBeenCalledWith('acme');
      expect(root.textContent).toContain('Acme Renamed');
    } finally {
      vi.useRealTimers();
    }
  });

  it('offers no Connect repository on a project from the server configuration', () => {
    const { root } = render(true, undefined, { role: 'Admin', permissions: EVERY_PERMISSION, managed: true });
    expect(root.textContent).not.toContain('Connect repository');
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
