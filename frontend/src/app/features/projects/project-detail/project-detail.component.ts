/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, computed, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, Router, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { AuthService } from '@core/services/auth.service';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { TasksService } from '@core/services/tasks.service';
import { StarsService } from '@core/services/stars.service';
import { NotifyService } from '@core/services/notify.service';
import { apiErrorMessage } from '@core/services/api.service';
import {
  ButtonComponent,
  CardGridComponent,
  LoadMoreComponent,
  DialogComponent,
  FormFieldComponent,
  InputDirective,
  LoadStateComponent,
  NavCardComponent,
  PageLayoutComponent,
  SubmitDirective,
} from '@gradient/ui/ui';
import { EvalStatusBadgeComponent, LabelHelpComponent, StarButtonComponent } from '@shared/ui';
import { repositoryUrlError } from '@shared/text';
import { NameFieldsComponent, fieldChecks, firstFieldError, nameAvailability } from '@shared/forms';
import { StarTarget, ProjectResponse, TaskResponse } from '@core/models';
import { ProjectAccessData } from '@core/resolvers/project-access.resolver';
import { links } from '@shared/routing';
import { pagedList } from '@shared/paging';

const RESERVED_TASK_NAMES = ['build-request'];

@Component({
  selector: 'app-project-detail',
  standalone: true,
  imports: [
    CommonModule,
    RouterModule,
    FormsModule,
    DialogComponent,
    ButtonComponent,
    InputDirective,
    LoadStateComponent,
    LabelHelpComponent,
    EvalStatusBadgeComponent,
    CardGridComponent,
    LoadMoreComponent,
    NavCardComponent,
    PageLayoutComponent,
    FormFieldComponent,
    StarButtonComponent,
    SubmitDirective,
    NameFieldsComponent,
  ],
  templateUrl: './project-detail.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class ProjectDetailComponent implements OnInit {
  protected readonly links = links;
  private route = inject(ActivatedRoute);
  protected authService = inject(AuthService);
  private tasksService = inject(TasksService);
  private stars = inject(StarsService);
  private crumbs = inject(BreadcrumbsService);
  private notify = inject(NotifyService);
  private router = inject(Router);
  protected names = nameAvailability(
    (name) => this.tasksService.checkTaskNameAvailable(this.projectName, name),
    RESERVED_TASK_NAMES,
  );

  project = signal<ProjectResponse>((this.route.snapshot.data['projectAccess'] as ProjectAccessData).project);
  canCreateTask = computed(() => this.project().permissions.includes('createTask'));
  tasks = pagedList<TaskResponse>('tasks', (page) => this.tasksService.getTasks(this.projectName, page));
  showCreateDialog = signal(false);
  creating = signal(false);
  createError = signal<string | null>(null);
  createAttempted = signal(false);
  repositoryTouched = signal(false);
  starred = signal(false);

  projectName = '';
  breadcrumb = computed(() => this.crumbs.project(this.projectName));
  starTarget: StarTarget = { kind: 'project', project: '' };

  newTask = {
    name: '',
    display_name: '',
    description: '',
    repository: '',
    wildcard: 'packages.x86_64-linux.**',
  };

  ngOnInit(): void {
    this.projectName = this.route.snapshot.paramMap.get('project') || '';
    this.starTarget = { kind: 'project', project: this.projectName };
    this.stars.starred(this.starTarget).subscribe((starred) => this.starred.set(starred));
    this.tasks.reload();
  }

  openCreateDialog(): void {
    this.newTask = { name: '', display_name: '', description: '', repository: '', wildcard: 'packages.x86_64-linux.**' };
    this.names.reset();
    this.createError.set(null);
    this.createAttempted.set(false);
    this.repositoryTouched.set(false);
    this.showCreateDialog.set(true);
  }

  get wildcardInvalid(): boolean {
    const w = this.newTask.wildcard.trim();
    if (!w) return false; // empty means use default - not invalid
    const parts = w.split(',').map((p) => p.trim());
    return parts.some((p) => !p || p.startsWith('.') || /\s/.test(p));
  }

  get repositoryError(): string | null {
    const url = this.newTask.repository.trim();
    if (!url) return this.createAttempted() || this.repositoryTouched() ? 'Enter the repository URL.' : null;
    return firstFieldError(url, fieldChecks.remoteRepository, (value) => repositoryUrlError(String(value)));
  }

  get missingNamesError(): string | null {
    const missing = !this.newTask.name || !this.newTask.display_name;
    return this.createAttempted() && missing ? 'Enter a display name and a name.' : null;
  }

  createTask(): void {
    if (this.creating() || this.names.blocksSubmit()) return;
    this.createAttempted.set(true);
    if (this.missingNamesError || this.repositoryError || this.wildcardInvalid) return;

    this.creating.set(true);
    this.createError.set(null);
    const created = this.newTask.name;
    this.tasksService
      .createTask(this.projectName, this.newTask)
      .subscribe({
        next: () => {
          this.creating.set(false);
          this.showCreateDialog.set(false);
          this.notify.success('Task created');
          this.router.navigateByUrl(links.task(this.projectName, created));
        },
        error: (error: unknown) => {
          this.createError.set(apiErrorMessage(error, 'Failed to create task.'));
          this.creating.set(false);
        },
      });
  }

}
