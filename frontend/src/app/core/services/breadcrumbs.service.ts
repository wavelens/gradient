/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, Signal, WritableSignal, inject, signal, untracked } from '@angular/core';
import type { Observable } from 'rxjs';
import type { Crumb } from '@gradient/ui/ui';
import { type BuildOrigin, links } from '@shared/routing';
import { CachesService } from './caches.service';
import { EvaluationsService } from './evaluations.service';
import { ProjectsService } from './projects.service';
import { TasksService } from './tasks.service';
import { TeamsService } from './teams.service';

interface Named {
  display_name: string;
}

/// All pages build the breadcrumb trail here. All pages then share the same hierarchy and show the display name of a project, task, cache, or team. Names stay null until known. The task link can lead back to the evaluation that was last selected on the task page.
@Injectable({ providedIn: 'root' })
export class BreadcrumbsService {
  private projects = inject(ProjectsService);
  private tasks = inject(TasksService);
  private caches = inject(CachesService);
  private teams = inject(TeamsService);
  private evaluationsService = inject(EvaluationsService);
  private names = new Map<string, WritableSignal<string | null>>();
  private taskUrlNameSignalByEvaluationId = new Map<string, Signal<string | null>>();
  private evaluations = new Map<string, string>();
  private taskSettingsAccessByTaskKey = new Map<string, boolean>();

  project(project: string, ...tail: Crumb[]): Crumb[] {
    const name = this.name(`project/${project}`, project, () => this.projects.getProject(project));

    return [{ label: 'Projects', link: links.projects() }, { label: name(), link: links.project(project) }, ...tail];
  }

  projectSettings(project: string, ...tail: Crumb[]): Crumb[] {
    return this.project(project, { label: 'Settings', link: links.projectPage(project, 'settings') }, ...tail);
  }

  task(project: string, task: string, ...tail: Crumb[]): Crumb[] {
    return this.taskAt(project, task, this.evaluations.get(`${project}/${task}`), ...tail);
  }

  taskAt(project: string, task: string, evaluation: string | null | undefined, ...tail: Crumb[]): Crumb[] {
    const name = this.name(`task/${project}/${task}`, task, () => this.tasks.getTaskInfo(project, task));

    return this.project(project, { label: name(), link: links.task(project, task, evaluation) }, ...tail);
  }

  taskSettings(project: string, task: string, ...tail: Crumb[]): Crumb[] {
    const canOpen = this.taskSettingsAccessByTaskKey.get(`${project}/${task}`) ?? true;

    return this.task(project, task, { label: 'Settings', link: canOpen ? links.taskPage(project, task, 'settings') : undefined }, ...tail);
  }

  buildPage(project: string, from: BuildOrigin, build: string, label: string): Crumb[] {
    if (from.evaluation) {
      const link = links.evaluationLog(project, from.evaluation, { build });
      return this.buildEvaluationBreadcrumbsWithLinkAndTail(project, from.evaluation, from.task, link, [{ label }]);
    }
    return from.task ? this.taskAt(project, from.task, null, { label }) : this.project(project, { label });
  }

  buildEvaluationPageBreadcrumbs(project: string, evaluation: string, task: string | null | undefined, ...tail: Crumb[]): Crumb[] {
    const link = tail.length ? links.evaluationLog(project, evaluation) : undefined;
    return this.buildEvaluationBreadcrumbsWithLinkAndTail(project, evaluation, task, link, tail);
  }

  private buildEvaluationBreadcrumbsWithLinkAndTail(
    project: string,
    evaluation: string,
    task: string | null | undefined,
    link: Crumb['link'],
    tail: Crumb[],
  ): Crumb[] {
    const crumb: Crumb = { label: 'Evaluation', link };
    const taskName = task || this.getEvaluationTaskUrlNameSignal(evaluation)();
    return taskName ? this.taskAt(project, taskName, evaluation, crumb, ...tail) : this.project(project, crumb, ...tail);
  }

  cache(cache: string, ...tail: Crumb[]): Crumb[] {
    const name = this.name(`cache/${cache}`, cache, () => this.caches.getCache(cache));

    return [{ label: 'Caches', link: links.caches() }, { label: name(), link: links.cache(cache) }, ...tail];
  }

  cacheSettings(cache: string, ...tail: Crumb[]): Crumb[] {
    return this.cache(cache, { label: 'Settings', link: links.cachePage(cache, 'settings') }, ...tail);
  }

  team(team: string, ...tail: Crumb[]): Crumb[] {
    const name = this.name(`team/${team}`, team, () => this.teams.get(team));

    return [{ label: 'Teams', link: links.teams() }, { label: name(), link: links.team(team) }, ...tail];
  }

  admin(...tail: Crumb[]): Crumb[] {
    return [{ label: 'Admin', link: links.admin('overview') }, ...tail];
  }

  rememberProject(project: string, displayName: string): void {
    this.remember(`project/${project}`, displayName);
  }

  rememberTask(project: string, task: string, displayName: string): void {
    this.remember(`task/${project}/${task}`, displayName);
  }

  rememberEvaluation(project: string, task: string, evaluation: string): void {
    this.evaluations.set(`${project}/${task}`, evaluation);
  }

  rememberTaskSettingsAccess(project: string, task: string, canOpen: boolean): void {
    this.taskSettingsAccessByTaskKey.set(`${project}/${task}`, canOpen);
  }

  rememberCache(cache: string, displayName: string): void {
    this.remember(`cache/${cache}`, displayName);
  }

  rememberTeam(team: string, displayName: string): void {
    this.remember(`team/${team}`, displayName);
  }

  getLastKnownDisplayNameSignal(kind: 'project' | 'task' | 'cache' | 'team', ...urlNames: string[]): Signal<string | null> | undefined {
    return this.names.get([kind, ...urlNames].join('/'));
  }

  private getEvaluationTaskUrlNameSignal(evaluation: string): Signal<string | null> {
    const known = this.taskUrlNameSignalByEvaluationId.get(evaluation);
    if (known) return known;
    const task = signal<string | null>(null);
    this.taskUrlNameSignalByEvaluationId.set(evaluation, task);
    untracked(() => this.evaluationsService.getEvaluation(evaluation).subscribe({
      next: (loaded) => task.set(loaded.task_name || null),
      error: () => {},
    }));

    return task;
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
