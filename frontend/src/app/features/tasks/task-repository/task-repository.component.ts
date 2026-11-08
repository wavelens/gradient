/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, OnInit, computed, inject, signal, viewChild } from '@angular/core';
import { ActivatedRoute } from '@angular/router';
import { LoadStateComponent, PageLayoutComponent } from '@gradient/ui/ui';
import { TaskRepositoryResponse } from '@core/models';
import { TasksService } from '@core/services/tasks.service';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { CanComponentDeactivate } from '@core/guards/unsaved-changes.guard';
import { RepositoryAccessComponent } from './repository-access.component';
import { RepositoryEventsComponent } from './repository-events.component';
import { repositoryHost } from '@shared/git';
import { requestIfProjectMember } from '@shared/access';
import { injectTaskAccessData } from '@core/resolvers/inject-access';

@Component({
  selector: 'app-task-repository',
  standalone: true,
  imports: [LoadStateComponent, PageLayoutComponent, RepositoryAccessComponent, RepositoryEventsComponent],
  template: `
    <gr-page-layout width="settings" [breadcrumb]="breadcrumb()" title="Repository"
      subtitle="Connect this task to the Git host for access, push-triggered builds and commit status.">
      @if (state(); as repo) {
        <app-repository-access [project]="projectName" [task]="taskName" [state]="repo"
          [gitHost]="gitHost()" (changed)="load()" />
        <app-repository-events [project]="projectName" [task]="taskName" [state]="repo" (saved)="state.set($event)" />
      } @else {
        <gr-load-state [loading]="loading()" [error]="loadError()" loadingMessage="Loading repository..."
          errorTitle="Failed to load repository" (retry)="load()" />
      }
    </gr-page-layout>
  `,
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class TaskRepositoryComponent implements OnInit, CanComponentDeactivate {
  private route = inject(ActivatedRoute);
  private tasks = inject(TasksService);
  private crumbs = inject(BreadcrumbsService);
  private taskAccess = injectTaskAccessData();

  private events = viewChild(RepositoryEventsComponent);

  projectName = '';
  taskName = '';
  state = signal<TaskRepositoryResponse | null>(null);
  loading = signal(true);
  loadError = signal<unknown>(null);

  breadcrumb = computed(() => this.crumbs.taskSettings(this.projectName, this.taskName, { label: 'Repository' }));
  gitHost = computed(() => {
    const repo = this.state();
    if (!repo) return null;
    const host = repositoryHost(repo.repository);
    const serving = repo.integrations.find((i) => i.serves_repository);
    return serving?.git_host_type ?? (host === 'gitlab.com' ? 'gitlab' : null);
  });

  ngOnInit(): void {
    this.projectName = this.route.snapshot.paramMap.get('project') ?? '';
    this.taskName = this.route.snapshot.paramMap.get('task') ?? '';
    this.load();
  }

  load(): void {
    this.loading.set(true);
    this.loadError.set(null);
    requestIfProjectMember(this.taskAccess()?.task, () => this.tasks.getRepository(this.projectName, this.taskName)).subscribe({
      next: (state) => {
        this.state.set(state);
        this.loading.set(false);
      },
      error: (error: unknown) => {
        this.loadError.set(error);
        this.loading.set(false);
      },
    });
  }

  canDeactivate() {
    return this.events()?.canLeave() ?? true;
  }
}
