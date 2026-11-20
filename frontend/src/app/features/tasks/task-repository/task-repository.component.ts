/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, OnInit, computed, inject, signal, viewChild } from '@angular/core';
import { ActivatedRoute } from '@angular/router';
import { Subscription } from 'rxjs';
import { LoadStateComponent, PageLayoutComponent } from '@gradient/ui/ui';
import { TaskRepositoryResponse } from '@core/models';
import { TasksService } from '@core/services/tasks.service';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { CanComponentDeactivate } from '@core/guards/unsaved-changes.guard';
import { repositoryHost } from '@shared/git';
import { SettingsAccessStateBadgeComponent, requestIfProjectMember } from '@shared/access';
import { injectTaskAccessData } from '@core/resolvers/inject-access';
import { nextChainStepKey } from '@shared/git-host-connections/task-git-host-chain.component';
import { TaskSettingsNavigationComponent } from '../task-settings-navigation/task-settings-navigation.component';
import { RepositoryAccessComponent } from './repository-access.component';
import { RepositoryGitHostConnectionComponent } from './repository-git-host-connection.component';
import { RepositoryIncomingEventsComponent } from './repository-incoming-events.component';
import { RepositoryCommitStatusComponent } from './repository-commit-status.component';

@Component({
  selector: 'app-task-repository',
  standalone: true,
  imports: [
    TaskSettingsNavigationComponent,
    LoadStateComponent,
    PageLayoutComponent,
    RepositoryAccessComponent,
    RepositoryGitHostConnectionComponent,
    RepositoryIncomingEventsComponent,
    RepositoryCommitStatusComponent,
    SettingsAccessStateBadgeComponent,
  ],
  template: `
    <gr-page-layout width="settings" [breadcrumb]="breadcrumb()" title="Repository">
      <app-task-settings-navigation slot="settings-navigation" [projectUrlName]="projectName" [taskUrlName]="taskName" />
      @if (state()) {
        <app-settings-access-state-badge slot="after-title" [access]="taskAccess()?.access" kind="task" showReadOnlyNote />
      }
      @if (state(); as repo) {
        <app-repository-access [project]="projectName" [task]="taskName" [state]="repo" [gitHost]="gitHost()"
          [hasPrimaryAction]="stepWithPrimaryAction() === 'repository'" (changed)="load()" />
        <app-repository-git-host-connection [project]="projectName" [task]="taskName" [state]="repo"
          [hasPrimaryAction]="stepWithPrimaryAction() === 'connection'" (saved)="state.set($event)" />
        <app-repository-incoming-events [project]="projectName" [task]="taskName" [state]="repo"
          [hasPrimaryAction]="stepWithPrimaryAction() === 'incoming_events'" (changed)="load()" />
        <app-repository-commit-status [project]="projectName" [task]="taskName" [state]="repo"
          [hasPrimaryAction]="stepWithPrimaryAction() === 'commit_status'" (saved)="state.set($event)" (changed)="load()" />
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
  protected taskAccess = injectTaskAccessData();

  private access = viewChild(RepositoryAccessComponent);
  private loadRequest?: Subscription;

  projectName = '';
  taskName = '';
  state = signal<TaskRepositoryResponse | null>(null);
  loading = signal(true);
  loadError = signal<unknown>(null);

  stepWithPrimaryAction = computed(() => {
    const repo = this.state();
    return repo ? nextChainStepKey(repo.chain) : undefined;
  });
  breadcrumb = computed(() => this.crumbs.taskSettings(this.projectName, this.taskName));
  gitHost = computed(() => {
    const repo = this.state();
    if (!repo) return null;
    const host = repositoryHost(repo.repository);
    return repo.git_host_connection?.git_host_type ?? (host === 'gitlab.com' ? 'gitlab' : null);
  });

  ngOnInit(): void {
    this.projectName = this.route.snapshot.paramMap.get('project') ?? '';
    this.taskName = this.route.snapshot.paramMap.get('task') ?? '';
    this.load();
  }

  load(): void {
    this.loading.set(true);
    this.loadError.set(null);
    this.loadRequest?.unsubscribe();
    this.loadRequest = requestIfProjectMember(this.taskAccess()?.task, () => this.tasks.getRepository(this.projectName, this.taskName)).subscribe({
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
    const unsaved = this.access()?.unsaved;
    return unsaved?.changed() ? unsaved.canLeave() : true;
  }
}
