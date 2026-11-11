/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, computed, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { AuthService } from '@core/services/auth.service';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { TasksService } from '@core/services/tasks.service';
import { StarsService } from '@core/services/stars.service';
import {
  BadgeComponent,
  ButtonComponent,
  CopyFieldComponent,
  CardGridComponent,
  LoadMoreComponent,
  LoadStateComponent,
  NavCardComponent,
  PageLayoutComponent,
  SettingsSectionComponent,
} from '@gradient/ui/ui';
import { EvalStatusBadgeComponent, StarButtonComponent, EntityNameComponent, ProjectReadinessCardComponent, entityKindToIconName, formatEntityLabel } from '@shared/ui';
import { CacheSubscriptionItem, StarTarget, ProjectResponse, TaskResponse } from '@core/models';
import { ProjectAccessData } from '@core/resolvers/project-access.resolver';
import { links } from '@shared/routing';
import { canOpenProjectSettings, isViewerProjectMember } from '@shared/access';
import { pagedList, runReloadIntervalWhileTabVisible } from '@shared/paging';
import { ProjectsService } from '@core/services/projects.service';
import { EMPTY, catchError, merge, tap } from 'rxjs';
import { CurrentTimeSignal, formatDuration, relativeTime, serverTime } from '@shared/text';
import { EvaluationHistoryComponent } from '@features/dashboard/evaluation-history/evaluation-history.component';

@Component({
  selector: 'app-project-detail',
  standalone: true,
  imports: [
    EntityNameComponent,
    CommonModule,
    RouterModule,
    ButtonComponent,
    CopyFieldComponent,
    LoadStateComponent,
    EvalStatusBadgeComponent,
    CardGridComponent,
    LoadMoreComponent,
    NavCardComponent,
    PageLayoutComponent,
    StarButtonComponent,
    BadgeComponent,
    ProjectReadinessCardComponent,
    SettingsSectionComponent,
    EvaluationHistoryComponent,
  ],
  templateUrl: './project-detail.component.html',
  styleUrl: './project-detail.component.scss',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class ProjectDetailComponent implements OnInit {
  protected readonly entityKindToIconName = entityKindToIconName;
  protected readonly formatEntityLabel = formatEntityLabel;
  protected readonly links = links;
  protected readonly isViewerProjectMember = isViewerProjectMember;
  private route = inject(ActivatedRoute);
  protected authService = inject(AuthService);
  private tasksService = inject(TasksService);
  private projectsService = inject(ProjectsService);
  private stars = inject(StarsService);
  private crumbs = inject(BreadcrumbsService);
  private now = inject(CurrentTimeSignal).currentTimeMilliseconds;

  project = signal<ProjectResponse>((this.route.snapshot.data['projectAccess'] as ProjectAccessData).project);
  canCreateTask = computed(() => !this.project().managed && this.project().permissions.includes('createTask'));
  canOpenSettings = computed(() => canOpenProjectSettings(this.project()));
  canOpenMembers = computed(() => isViewerProjectMember(this.project()) || this.authService.user()?.superuser === true);
  tasks = pagedList<TaskResponse>('tasks', (page) => this.tasksService.getTasks(this.projectName, page, 50));
  starred = signal(false);
  cacheSubscriptions = signal<CacheSubscriptionItem[] | null>(null);
  activeBinaryCacheNames = computed(() => (this.cacheSubscriptions() ?? []).filter((c) => c.status === 'active').map((c) => c.display_name));

  projectName = '';
  breadcrumb = computed(() => this.crumbs.project(this.projectName));
  starTarget: StarTarget = { kind: 'project', project: '' };

  constructor() {
    runReloadIntervalWhileTabVisible(() =>
      merge(
        this.tasks.refreshFirstPageSilently(),
        this.projectsService.getProject(this.projectName).pipe(
          tap((project) => this.project.set(project)),
          catchError(() => EMPTY),
        ),
      ),
    );
  }

  protected getLatestEvaluationStatusText(task: TaskResponse): string | null {
    const latest = task.recent_evaluations.at(-1);
    if (!latest) return null;
    const now = this.now();
    if (latest.finished_at) return `Finished ${relativeTime(latest.finished_at, now)}`;
    if (latest.started_at) return `Running for ${formatDuration(now - serverTime(latest.started_at).getTime())}`;
    return `Queued ${relativeTime(latest.created_at, now)}`;
  }

  ngOnInit(): void {
    this.projectName = this.route.snapshot.paramMap.get('project') || '';
    this.starTarget = { kind: 'project', project: this.projectName };
    this.stars.starred(this.starTarget).subscribe((starred) => this.starred.set(starred));
    this.tasks.reload();
    if (isViewerProjectMember(this.project())) {
      this.projectsService
        .getSubscribedCaches(this.projectName)
        .pipe(catchError(() => EMPTY))
        .subscribe((caches) => this.cacheSubscriptions.set(caches));
    }
  }
}
