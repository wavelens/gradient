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
  ButtonComponent,
  CardGridComponent,
  LoadMoreComponent,
  LoadStateComponent,
  NavCardComponent,
  PageLayoutComponent,
} from '@gradient/ui/ui';
import { EvalStatusBadgeComponent, StarButtonComponent } from '@shared/ui';
import { StarTarget, ProjectResponse, TaskResponse } from '@core/models';
import { ProjectAccessData } from '@core/resolvers/project-access.resolver';
import { links } from '@shared/routing';
import { canOpenProjectSettings, isViewerProjectMember } from '@shared/access';
import { pagedList, runReloadIntervalWhileTabVisible } from '@shared/paging';
import { ProjectsService } from '@core/services/projects.service';
import { EMPTY, catchError } from 'rxjs';

@Component({
  selector: 'app-project-detail',
  standalone: true,
  imports: [
    CommonModule,
    RouterModule,
    ButtonComponent,
    LoadStateComponent,
    EvalStatusBadgeComponent,
    CardGridComponent,
    LoadMoreComponent,
    NavCardComponent,
    PageLayoutComponent,
    StarButtonComponent,
  ],
  templateUrl: './project-detail.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class ProjectDetailComponent implements OnInit {
  protected readonly links = links;
  private route = inject(ActivatedRoute);
  protected authService = inject(AuthService);
  private tasksService = inject(TasksService);
  private projectsService = inject(ProjectsService);
  private stars = inject(StarsService);
  private crumbs = inject(BreadcrumbsService);

  project = signal<ProjectResponse>((this.route.snapshot.data['projectAccess'] as ProjectAccessData).project);
  canCreateTask = computed(() => this.project().permissions.includes('createTask'));
  canOpenSettings = computed(() => canOpenProjectSettings(this.project()));
  tasks = pagedList<TaskResponse>('tasks', (page) => this.tasksService.getTasks(this.projectName, page, 50));
  starred = signal(false);
  activeBinaryCacheNames = signal<string[]>([]);

  projectName = '';
  breadcrumb = computed(() => this.crumbs.project(this.projectName));
  starTarget: StarTarget = { kind: 'project', project: '' };

  constructor() {
    runReloadIntervalWhileTabVisible(() => this.tasks.refreshFirstPageSilently());
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
        .subscribe((caches) => this.activeBinaryCacheNames.set(caches.filter((c) => c.status === 'active').map((c) => c.name)));
    }
  }
}
