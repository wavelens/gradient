/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, Signal, WritableSignal, inject, signal, untracked } from '@angular/core';
import type { Observable } from 'rxjs';
import type { Crumb } from '@gradient/ui/ui';
import { CachesService } from './caches.service';
import { ProjectsService } from './projects.service';
import { TasksService } from './tasks.service';
import { TeamsService } from './teams.service';

interface Named {
  display_name: string;
}

/// Every page builds its trail here, so all pages share the levels and show the
/// display name of a project, task, cache or team. A name is `null` until known.
@Injectable({ providedIn: 'root' })
export class BreadcrumbsService {
  private projects = inject(ProjectsService);
  private tasks = inject(TasksService);
  private caches = inject(CachesService);
  private teams = inject(TeamsService);
  private names = new Map<string, WritableSignal<string | null>>();

  project(project: string, ...tail: Crumb[]): Crumb[] {
    const name = this.name(`project/${project}`, project, () => this.projects.getProject(project));

    return [{ label: 'Projects', link: ['/projects'] }, { label: name(), link: ['/project', project] }, ...tail];
  }

  projectSettings(project: string, ...tail: Crumb[]): Crumb[] {
    return this.project(project, { label: 'Settings', link: ['/project', project, 'settings'] }, ...tail);
  }

  task(project: string, task: string, ...tail: Crumb[]): Crumb[] {
    const name = this.name(`task/${project}/${task}`, task, () => this.tasks.getTaskInfo(project, task));

    return this.project(project, { label: name(), link: ['/project', project, 'task', task] }, ...tail);
  }

  taskSettings(project: string, task: string, ...tail: Crumb[]): Crumb[] {
    return this.task(project, task, { label: 'Settings', link: ['/project', project, 'task', task, 'settings'] }, ...tail);
  }

  cache(cache: string, ...tail: Crumb[]): Crumb[] {
    const name = this.name(`cache/${cache}`, cache, () => this.caches.getCache(cache));

    return [{ label: 'Caches', link: ['/caches'] }, { label: name(), link: ['/caches', cache] }, ...tail];
  }

  cacheSettings(cache: string, ...tail: Crumb[]): Crumb[] {
    return this.cache(cache, { label: 'Settings', link: ['/caches', cache, 'settings'] }, ...tail);
  }

  team(team: string, ...tail: Crumb[]): Crumb[] {
    const name = this.name(`team/${team}`, team, () => this.teams.get(team));

    return [{ label: 'Teams', link: ['/teams'] }, { label: name(), link: ['/team', team] }, ...tail];
  }

  rememberProject(project: string, displayName: string): void {
    this.remember(`project/${project}`, displayName);
  }

  rememberTask(project: string, task: string, displayName: string): void {
    this.remember(`task/${project}/${task}`, displayName);
  }

  rememberCache(cache: string, displayName: string): void {
    this.remember(`cache/${cache}`, displayName);
  }

  rememberTeam(team: string, displayName: string): void {
    this.remember(`team/${team}`, displayName);
  }

  private remember(key: string, displayName: string): void {
    const known = this.names.get(key);
    if (known) known.set(displayName);
    else this.names.set(key, signal(displayName));
  }

  // Trails are read inside `computed`, where a signal write is only legal untracked.
  private name(key: string, urlName: string, load: () => Observable<Named>): Signal<string | null> {
    const known = this.names.get(key);
    if (known) return known;
    const name = signal<string | null>(null);
    this.names.set(key, name);
    untracked(() => load().subscribe({
      next: (entity) => name.set(entity.display_name),
      error: () => name.set(urlName),
    }));

    return name;
  }
}
